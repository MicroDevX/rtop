use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Chart, Dataset, Gauge, GraphType, LineGauge, Paragraph, Row,
        Sparkline, Table, TableState,
    },
};

use sysinfo::{Disks, ProcessesToUpdate, System};

// معدل التحديث: مرتان في الثانية (500 ميلي ثانية)
const TICK_RATE: Duration = Duration::from_millis(500);
// الاحتفاظ بـ 120 قراءة (تغطي 60 ثانية)
const HISTORY_LEN: usize = 120;

// الألوان المستخدمة في الواجهة
const MUTED: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const RAM_COLOR: Color = Color::Magenta;

#[derive(PartialEq)]
enum SortBy {
    Cpu,
    Memory,
}

struct ProcessRow {
    pid: u32,
    name: String,
    cpu: f32,
    memory_mb: u64,
}

struct App {
    running: bool,
    system: System,
    disks: Disks,
    hostname: String,
    cpu_history: VecDeque<f64>,
    ram_history: VecDeque<f64>,
    processes: Vec<ProcessRow>,
    t_state: TableState,
    sort_by: SortBy,
}

impl App {
    fn new() -> Self {
        let mut system = System::new_all();
        thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_cpu_usage();

        let disks = Disks::new_with_refreshed_list();
        let hostname = System::host_name().unwrap_or_else(|| "unknown".into());

        let mut app = Self {
            running: true,
            system,
            disks,
            hostname,
            cpu_history: VecDeque::with_capacity(HISTORY_LEN),
            ram_history: VecDeque::with_capacity(HISTORY_LEN),
            processes: Vec::new(),
            t_state: TableState::default().with_selected(Some(0)),
            sort_by: SortBy::Cpu,
        };

        app.on_tick();
        app
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        let mut last_tick = Instant::now();

        while self.running {
            terminal.draw(|frame| self.render(frame))?;

            let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key);
                }
            }

            if last_tick.elapsed() >= TICK_RATE {
                self.on_tick();
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Down | KeyCode::Char('j') => {
                let i = match self.t_state.selected() {
                    Some(i) => {
                        if i >= self.processes.len().saturating_sub(1) {
                            0
                        } else {
                            i + 1
                        }
                    }
                    None => 0,
                };
                self.t_state.select(Some(i));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = match self.t_state.selected() {
                    Some(i) => {
                        if i == 0 {
                            self.processes.len().saturating_sub(1)
                        } else {
                            i - 1
                        }
                    }
                    None => 0,
                };
                self.t_state.select(Some(i));
            }
            KeyCode::Char('c') => {
                self.sort_by = SortBy::Cpu;
                self.sort_processes();
            }
            KeyCode::Char('m') => {
                self.sort_by = SortBy::Memory;
                self.sort_processes();
            }
            _ => {}
        }
    }

    fn on_tick(&mut self) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.disks.refresh(true);
        self.system.refresh_processes(ProcessesToUpdate::All, true);

        let cpu_global = self.system.global_cpu_usage() as f64;
        push_history(&mut self.cpu_history, cpu_global);

        let ram_pct = calc_percent(self.system.used_memory(), self.system.total_memory());
        push_history(&mut self.ram_history, ram_pct);

        self.collect_processes();
    }

    fn collect_processes(&mut self) {
        let cores = (self.system.cpus().len().max(1)) as f32;
        self.processes = self
            .system
            .processes()
            .values()
            .map(|p| ProcessRow {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage() / cores,
                memory_mb: p.memory() / 1024 / 1024,
            })
            .collect();

        self.sort_processes();
    }

    fn sort_processes(&mut self) {
        match self.sort_by {
            SortBy::Cpu => self.processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu)),
            SortBy::Memory => self.processes.sort_by(|a, b| b.memory_mb.cmp(&a.memory_mb)),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        // تقسيم الشاشة الرئيسي: رأس، قسم علوي (CPU + Memory)، رسم بياني، جدول العمليات، تذييل
        let main_layout = Layout::vertical([
            Constraint::Length(3),  // Header
            Constraint::Length(12), // Top Panels (CPU & Memory side-by-side)
            Constraint::Length(9),  // History Chart
            Constraint::Fill(1),    // Processes Table
            Constraint::Length(1),  // Footer
        ])
        .split(frame.area());

        // تقسيم القسم العلوي إلى قسمين متساويين أفقياً (CPU و Memory)
        let top_layout =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(main_layout[1]);

        self.render_header(frame, main_layout[0]);
        self.render_cpu(frame, top_layout[0]);
        self.render_memory(frame, top_layout[1]);
        self.render_history(frame, main_layout[2]);
        self.render_processes(frame, main_layout[3]);
        self.render_footer(frame, main_layout[4]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let sep = Span::styled(" | ", Style::default().fg(MUTED));
        let uptime = System::uptime();
        let hours = uptime / 3600;
        let minutes = (uptime % 3600) / 60;

        let line = Line::from(vec![
            Span::raw(" 🚀 Ratatui Monitor ").bold().fg(ACCENT),
            sep.clone(),
            Span::raw(&self.hostname).bold(),
            sep,
            Span::raw(format!("Up: {}h {}m", hours, minutes)),
        ]);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(MUTED);

        frame.render_widget(Paragraph::new(line).block(block), area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let make_key = |k: &'static str, desc: &'static str| -> [Span; 2] {
            [
                Span::styled(
                    format!(" {k} "),
                    Style::default().fg(Color::Black).bg(ACCENT),
                ),
                Span::styled(format!(" {desc}  "), Style::default().fg(MUTED)),
            ]
        };

        let spans: Vec<Span> = [
            make_key("Q/Esc", "Quit"),
            make_key("J/Down", "Scroll Down"),
            make_key("K/Up", "Scroll Up"),
            make_key("C", "Sort CPU"),
            make_key("M", "Sort RAM"),
        ]
        .concat();

        frame.render_widget(Line::from(spans), area);
    }

    fn render_cpu(&mut self, frame: &mut Frame, area: Rect) {
        let avg = self.system.global_cpu_usage();
        let block = panel(format!("CPU Usage ({avg:.0}%)"));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // تقسيم مساحة الـ CPU الداخلية إلى Sparkline بالأعلى وخطوط الأنوية بالأسفل
        let chunks = Layout::vertical([
            Constraint::Length(3), // Sparkline chart
            Constraint::Fill(1),   // Cores gauges
        ])
        .split(inner);

        // عرض Sparkline للاستخدام العام
        let width = chunks[0].width as usize;
        let skip = self.cpu_history.len().saturating_sub(width);
        let data: Vec<u64> = self
            .cpu_history
            .iter()
            .skip(skip)
            .map(|&v| v as u64)
            .collect();

        let sparkline = Sparkline::default()
            .data(&data)
            .max(100)
            .style(Style::default().fg(ACCENT));
        frame.render_widget(sparkline, chunks[0]);

        // عرض أنوية المعالج بشكل مرتب (عمودين)
        let cpus = self.system.cpus();
        let half = (cpus.len() + 1) / 2;
        let core_cols =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(1)
                .split(chunks[1]);

        for (i, cpu) in cpus.iter().enumerate() {
            let usage = cpu.cpu_usage() as f64;
            let gauge = LineGauge::default()
                .ratio((usage / 100.0).clamp(0.0, 1.0))
                .label(format!("{:>2}: {:>3.0}%", i, usage))
                .filled_symbol("█")
                .unfilled_symbol("░")
                .filled_style(level_color(usage))
                .unfilled_style(Style::default().fg(MUTED));

            let col_idx = if i < half { 0 } else { 1 };
            let row_idx = if i < half { i } else { i - half };

            let row_rect = Rect {
                x: core_cols[col_idx].x,
                y: core_cols[col_idx].y + row_idx as u16,
                width: core_cols[col_idx].width,
                height: 1,
            };

            if row_rect.y < core_cols[col_idx].y + core_cols[col_idx].height {
                frame.render_widget(gauge, row_rect);
            }
        }
    }

    fn render_memory(&mut self, frame: &mut Frame, area: Rect) {
        let block = panel("Memory & Storage".into());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let slots = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .spacing(1)
        .split(inner);

        let ram_gauge = usage_gauge("RAM", self.system.used_memory(), self.system.total_memory());
        frame.render_widget(ram_gauge, slots[0]);

        let swap_gauge = usage_gauge("Swap", self.system.used_swap(), self.system.total_swap());
        frame.render_widget(swap_gauge, slots[1]);

        if let Some(disk) = self.disks.list().first() {
            let total = disk.total_space();
            let used = total.saturating_sub(disk.available_space());
            let label = disk.mount_point().to_string_lossy();
            let disk_gauge = usage_gauge(&label, used, total);
            frame.render_widget(disk_gauge, slots[2]);
        }
    }

    fn render_history(&mut self, frame: &mut Frame, area: Rect) {
        let cpu_pts = two_points(&self.cpu_history);
        let ram_pts = two_points(&self.ram_history);

        let datasets = vec![
            Dataset::default()
                .name("CPU %")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(ACCENT))
                .data(&cpu_pts),
            Dataset::default()
                .name("RAM %")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(RAM_COLOR))
                .data(&ram_pts),
        ];

        let x_axis = Axis::default()
            .bounds([0.0, (HISTORY_LEN - 1) as f64])
            .labels(["60s ago", "30s", "now"])
            .style(Style::default().fg(MUTED));

        let y_axis = Axis::default()
            .bounds([0.0, 100.0])
            .labels(["0%", "50%", "100%"])
            .style(Style::default().fg(MUTED));

        let chart = Chart::new(datasets)
            .block(panel("History (Last 60s)".into()))
            .x_axis(x_axis)
            .y_axis(y_axis);

        frame.render_widget(chart, area);
    }

    fn render_processes(&mut self, frame: &mut Frame, area: Rect) {
        let sort_name = match self.sort_by {
            SortBy::Cpu => "CPU",
            SortBy::Memory => "Memory",
        };

        let rows: Vec<Row> = self
            .processes
            .iter()
            .map(|p| {
                Row::new(vec![
                    p.pid.to_string(),
                    p.name.clone(),
                    format!("{:.1}%", p.cpu),
                    format!("{} MB", p.memory_mb),
                ])
            })
            .collect();

        let header = Row::new(vec!["PID", "Name", "CPU", "Memory"])
            .style(Style::default().fg(ACCENT).bold());

        let widths = [
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(12),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(panel(format!("Processes (Sorted by {sort_name})")))
            .row_highlight_style(Style::default().fg(Color::White).bg(Color::DarkGray).bold())
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(table, area, &mut self.t_state);
    }
}

fn panel(title: String) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTED))
        .title(Line::from(title).bold().fg(ACCENT))
}

fn level_color(percent: f64) -> Color {
    if percent < 50.0 {
        Color::Green
    } else if percent < 80.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn calc_percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    }
}

fn usage_gauge<'a>(name: &str, used: u64, total: u64) -> Gauge<'a> {
    let pct = calc_percent(used, total);
    let used_gb = used as f64 / 1024.0 / 1024.0 / 1024.0;
    let total_gb = total as f64 / 1024.0 / 1024.0 / 1024.0;

    let ratio = if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
    };

    Gauge::default()
        .ratio(ratio.clamp(0.0, 1.0))
        .label(format!("{name}: {used_gb:.1}/{total_gb:.1} GB ({pct:.0}%)"))
        .gauge_style(Style::default().fg(level_color(pct)).bg(Color::Black))
        .use_unicode(true)
}

fn push_history(history: &mut VecDeque<f64>, value: f64) {
    history.push_back(value);
    if history.len() > HISTORY_LEN {
        history.pop_front();
    }
}

fn two_points(history: &VecDeque<f64>) -> Vec<(f64, f64)> {
    let offset = (HISTORY_LEN.saturating_sub(history.len())) as f64;
    history
        .iter()
        .enumerate()
        .map(|(i, &v)| (offset + i as f64, v))
        .collect()
}

fn main() -> std::io::Result<()> {
    // إعداد التيرمينال عبر Ratatui
    let mut terminal = ratatui::init();

    // إنشاء التطبيق وتشغيله مع تمرير الـ terminal بشكل صحيح
    let mut app = App::new();
    let result = app.run(&mut terminal);

    // استعادة شكل التيرمينال الطبيعي عند الخروج
    ratatui::restore();
    result
}

// دالة التوافق مع DefaultTerminal الخاص بـ Ratatui
fn main_terminal(terminal: &mut DefaultTerminal) -> &mut DefaultTerminal {
    terminal
}
