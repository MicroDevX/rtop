mod app;
use app::App;

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
