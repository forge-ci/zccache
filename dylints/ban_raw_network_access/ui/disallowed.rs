// Every route to a transport: path, method, UFCS, alias, function item and a
// trait method instantiated on a transport type.
mod reqwest {
    pub struct Client;

    impl Client {
        pub fn builder() -> Client {
            unimplemented!()
        }

        pub fn head(&self) {}
    }

    impl Default for Client {
        fn default() -> Self {
            unimplemented!()
        }
    }

    pub fn get() {}
}

use reqwest::Client as Alias;

fn main() {
    let client = reqwest::Client::builder();
    client.head();
    reqwest::Client::head(&client);
    let item = Alias::builder;
    let _ = item();
    reqwest::get();
    let _ = Alias::default();
}
