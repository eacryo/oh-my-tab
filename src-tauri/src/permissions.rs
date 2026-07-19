use std::process;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXMakeProcessTrusted() -> bool;
}

pub fn check_accessibility_permission() {
    let trusted = unsafe { AXIsProcessTrusted() };
    if trusted {
        println!("[permissions] Accessibility permission granted");
        return;
    }

    eprintln!("[permissions] Accessibility permission required");
    eprintln!("[permissions] Requesting permission via system prompt...");

    unsafe { AXMakeProcessTrusted() };
    println!("[permissions] System prompt displayed, waiting for user...");

    for i in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if unsafe { AXIsProcessTrusted() } {
            println!("[permissions] Accessibility permission granted");
            return;
        }
        if i % 5 == 4 {
            eprintln!("[permissions] Waiting for permission... ({}/30)", i + 1);
        }
    }

    eprintln!("[permissions] Permission denied after 30s. Exiting.");
    process::exit(1);
}
