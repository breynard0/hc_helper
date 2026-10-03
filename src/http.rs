use std::{sync::LazyLock, time::Duration};

use reqwest::Client;

pub(crate) static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client")
});
