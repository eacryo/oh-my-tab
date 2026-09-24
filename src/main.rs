//! Thin bin entry: the implementation lives in the lib crate (oh_my_tab); the bin only
//! calls run().

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if oh_my_tab::run_relaunch_helper_if_requested(&args) {
        return;
    }
    oh_my_tab::run();
}
