mod model;
use raw_window_handle::HasWindowHandle;
use slint::ComponentHandle;
slint::include_modules!();
fn main() {
    let w = MainWindow::new().unwrap();
    w.show().unwrap();
    let weak = w.as_weak();
    slint::invoke_from_event_loop(move || {
        let w = weak.unwrap();
        let h = w.window().window_handle();
        println!("{:?}", h.window_handle().map(|x| x.as_raw()));
        if std::env::var("NOMICA").is_err() { println!("mica: {:?}", window_vibrancy::apply_mica(&h, None)); }
    }).unwrap();
    slint::run_event_loop().unwrap();
}
