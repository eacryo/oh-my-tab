//! oh-my-tab 薄壳入口:全部实现位于 libcrate(oh_my_tab),bin 仅负责调用 run()。
//! Thin bin entry: the implementation lives in the lib crate (oh_my_tab); the bin only
//! calls run().

fn main() {
    oh_my_tab::run();
}
