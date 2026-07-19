#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub fn check_accessibility_permission() {
    let trusted = unsafe { AXIsProcessTrusted() };
    if trusted {
        println!("[permissions] Accessibility permission granted");
        return;
    }

    eprintln!("============================================");
    eprintln!("[permissions] Accessibility permission required!");
    eprintln!("[permissions]");
    eprintln!("[permissions] Please go to:");
    eprintln!("[permissions]   System Settings → Privacy & Security → Accessibility");
    eprintln!("[permissions]");
    eprintln!("[permissions] Find this app in the list and enable the toggle.");
    eprintln!("[permissions] Then restart the application.");
    eprintln!("============================================");
    eprintln!("");
    eprintln!("[permissions] Continuing without permission — window list will be empty.");
}
