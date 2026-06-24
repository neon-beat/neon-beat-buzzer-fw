use embassy_net::Runner;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::{Config, Interface, WifiController, sta::StationConfig};
use log::info;

const SSID: &str = env!("NBC_SSID");
const PASSWORD: &str = env!("NBC_PASSWORD");

const RECONNECT_DELAY_MS: u64 = 5000;
const RADIO_RETRY_DELAY_MS: u64 = 1000;

#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
pub async fn connection(mut controller: WifiController<'static>) {
    loop {
        if controller.is_connected() {
            // wait until we're no longer connected
            let _ = controller.wait_for_disconnect_async().await;
            Timer::after(Duration::from_millis(RECONNECT_DELAY_MS)).await
        }

        let station_config = Config::Station(
            StationConfig::default()
                .with_ssid(SSID)
                .with_password(PASSWORD.into()),
        );
        if let Err(e) = controller.set_config(&station_config) {
            info!("Failed to configure radio stack: {e:?}, retrying...");
            Timer::after(Duration::from_millis(RADIO_RETRY_DELAY_MS)).await;
            continue;
        }
        info!("Connecting to NBC access point...");

        match controller.connect_async().await {
            Ok(_) => info!("Connected to NBC access point"),
            Err(e) => {
                info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(RECONNECT_DELAY_MS)).await
            }
        }
    }
}
