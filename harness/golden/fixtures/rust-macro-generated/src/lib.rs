macro_rules! make_answer {
    () => {
        pub fn answer() -> u32 {
            42
        }
    };
}

make_answer!();

pub fn ask() -> u32 {
    answer()
}
