mod widget;

use crate::widget::Widget;

pub fn main_run() {
    let w = Widget::new();
    w.run();
}
