pub mod net;

use crate::net::tcp::connect;

pub fn boot() {
    connect();
}
