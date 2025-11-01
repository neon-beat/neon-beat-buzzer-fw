use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Sender};
use embassy_time::Timer;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Pull};
use log::info;

#[embassy_executor::task]
pub async fn button_task(pin: AnyPin<'static>, sender: Sender<'static, NoopRawMutex, bool, 1>) {
    let config = InputConfig::default().with_pull(Pull::Up);
    let mut pushed = false;
    let mut button = Input::new(pin, config);
    loop {
        button.wait_for_falling_edge().await;
        if !pushed {
            info!("Button pushed !");
            sender.send(true).await;
        }
        /* Quick and dirty deboucing, enough as long as we only need to
         * detect single, short presses
         */
        Timer::after_millis(100).await;
        pushed = button.is_low();
    }
}
