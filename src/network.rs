use embassy_net::Runner;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::{
    ClientConfig, ModeConfig, WifiController, WifiDevice, WifiEvent, WifiStaState,
};
use log::{debug, info};

const SSID: &str = env!("NBC_SSID");
const PASSWORD: &str = env!("NBC_PASSWORD");

#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
pub async fn connection(mut controller: WifiController<'static>) {
    debug!("Device capabilities: {:?}", controller.capabilities());
    loop {
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = ModeConfig::Client(
                ClientConfig::default()
                    .with_ssid(SSID.into())
                    .with_password(PASSWORD.into()),
            );
            if let Err(e) = controller.set_config(&client_config) {
                info!("Failed to configure radio stack: {e:?}, retrying...");
                Timer::after(Duration::from_millis(1000)).await;
                continue;
            }
            info!("Starting wifi...");
            if let Err(e) = controller.start_async().await {
                info!("Failed to start radio stack: {e:?}, retrying...");
                Timer::after(Duration::from_millis(1000)).await;
                continue;
            }
            info!("Wifi started");
        }
        info!("Connecting to NBC access point...");

        match controller.connect_async().await {
            Ok(_) => info!("Connected to NBC access point"),
            Err(e) => {
                info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}
