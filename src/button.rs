use core::cell::RefCell;
use critical_section::Mutex;
use esp_hal::{
    gpio::{AnyPin, Event, Input, InputConfig, Pull},
    handler, ram,
};

static BUTTON: Mutex<RefCell<Option<Input>>> = Mutex::new(RefCell::new(None));

#[embassy_executor::task]
pub async fn button_task(pin: AnyPin<'static>) {
    let config = InputConfig::default().with_pull(Pull::Up);
    let mut button = Input::new(pin, config);
    critical_section::with(|cs| {
        button.listen(Event::FallingEdge);
        BUTTON.borrow_ref_mut(cs).replace(button)
    });

    loop {
        todo!("Button debounce logic");
    }
}

#[handler]
#[ram]
pub fn button_interrupt_handler() {
    if critical_section::with(|cs| {
        BUTTON
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .is_interrupt_set()
    }) {
        esp_println::println!("Button was the source of the interrupt");
    } else {
        esp_println::println!("Button was not the source of the interrupt");
    }

    critical_section::with(|cs| {
        BUTTON
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .clear_interrupt()
    });
}
