// An unrelated client and a reasoned exception.
mod http {
    pub struct Client;

    impl Client {
        pub fn get(&self) {}
    }
}

mod reqwest {
    pub fn get() {}
}

fn main() {
    http::Client.get();

    #[allow(ban_raw_network_access, reason = "fixture for a reasoned exception")]
    reqwest::get();
}
