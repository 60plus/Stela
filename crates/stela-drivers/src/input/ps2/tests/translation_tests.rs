//! A set-2 keyboard behind an i8042 the firmware left translating: the model
//! controller hands its bytes over in set 1, as the hardware does.
use super::*;

/// A firmware configuration with translation, the system flag and both
/// interrupts on, as notebooks leave it.
const TRANSLATING_FIRMWARE: u8 = TRANSLATION | 0x04 | IRQ_BITS;
/// That configuration as the start reads it, after disabling both ports.
const READ_WITH_PORTS_OFF: u8 = TRANSLATING_FIRMWARE | KEYBOARD_DISABLED | MOUSE_DISABLED;

fn translated(mouse: bool) -> (Controller<Model, Time>, Rc<RefCell<Hardware>>) {
    let (mut c, h) = controller(Config {
        mouse,
        reset_on_start: false,
        query_scan_set: false,
        ..Config::default()
    });
    h.borrow_mut().config = TRANSLATING_FIRMWARE;
    c.initialize().unwrap();
    (c, h)
}

fn key(usage: u8, pressed: bool) -> Input {
    Input::Key { usage, pressed }
}

#[test]
fn translation_is_kept_as_the_firmware_left_it_and_no_scan_set_is_sent() {
    for mouse in [false, true] {
        let (mut c, h) = translated(mouse);
        assert!(c.is_ready());
        assert_eq!(c.translated(), Some(true));
        assert_eq!(c.firmware_config(), Some(READ_WITH_PORTS_OFF));
        assert_eq!(h.borrow().config & (TRANSLATION | IRQ_BITS), TRANSLATION);
        assert!(!h.borrow().writes.contains(&(false, SET_SCAN)));
        // The keyboard answered GETID with AB 41, its ID translated.
        assert!(c.refused_commands().is_empty());
        assert!(h.borrow().output.is_empty());
        c.stop().unwrap();
        assert_ne!(h.borrow().config & TRANSLATION, 0);
    }
    // Untranslated as the firmware left it: Set 2 is selected.
    let (mut c, h) = controller(Config {
        mouse: false,
        reset_on_start: false,
        query_scan_set: false,
        ..Config::default()
    });
    c.initialize().unwrap();
    assert_eq!(c.translated(), Some(false));
    assert_eq!(h.borrow().config & TRANSLATION, 0);
    assert!(h.borrow().writes.contains(&(false, SET_SCAN)));
}

#[test]
fn translated_keys_decode_as_linux_atkbd_decodes_set_one() {
    let (mut c, h) = translated(true);
    // Each case is what the keyboard sends in set 2, and the inputs.
    let cases: [(&[u8], &[Input]); 10] = [
        // A: 1E, 9E.
        (&[0x1c, 0xf0, 0x1c], &[key(4, true), key(4, false)]),
        // Left shift: 2A, then AA, also a keyboard's BAT.
        (&[0x12, 0xf0, 0x12], &[key(0xe1, true), key(0xe1, false)]),
        // Keypad comma: 7E, then FE, also a NAK.
        (&[0x6d, 0xf0, 0x6d], &[key(0x85, true), key(0x85, false)]),
        // F7, set 2's code above 7F: 41, C1.
        (&[0x83, 0xf0, 0x83], &[key(0x40, true), key(0x40, false)]),
        // Right control: E0 1D, E0 9D.
        (
            &[0xe0, 0x14, 0xe0, 0xf0, 0x14],
            &[key(0xe4, true), key(0xe4, false)],
        ),
        // Right arrow: E0 4D, E0 CD.
        (
            &[0xe0, 0x74, 0xe0, 0xf0, 0x74],
            &[key(0x4f, true), key(0x4f, false)],
        ),
        // Print Screen and its fake shift: E0 2A E0 37, then E0 B7 E0 AA.
        (&[0xe0, 0x12, 0xe0, 0x7c], &[key(0x46, true)]),
        (&[0xe0, 0xf0, 0x7c, 0xe0, 0xf0, 0x12], &[key(0x46, false)]),
        // Pause: E1 1D 45, then E1 9D C5.
        (
            &[0xe1, 0x14, 0x77, 0xe1, 0xf0, 0x14, 0xf0, 0x77],
            &[key(0x48, true), key(0x48, false)],
        ),
        // Hanja sends no release; it is released at once.
        (&[0xf1], &[key(0x91, true), key(0x91, false)]),
    ];
    for (set2, expected) in cases {
        emit(&h, Port::Keyboard, set2);
        assert_eq!(c.poll().unwrap(), expected, "{set2:02x?}");
    }
    assert_eq!(c.framing_errors(), 0);
    // With no left shift held, AA is the keyboard's BAT; a lone ACK, NAK
    // or overrun is no key, and the keys after them decode.
    emit(&h, Port::Keyboard, &[0xaa, 0xfa, 0xfe, 0xff, 0xe0, 0x74]);
    assert_eq!(c.poll().unwrap(), [key(0x4f, true)]);
    assert_eq!(c.framing_errors(), 1);
    assert!(c.is_ready());
}

#[test]
fn translated_led_replies_are_replies_and_a_key_between_them_is_kept() {
    let (mut c, h) = translated(false);
    h.borrow_mut().inject_led = vec![(Port::Keyboard, 0x1c)];
    c.set_leds(4).unwrap();
    assert_eq!(h.borrow().leds, 4);
    assert_eq!(c.poll().unwrap(), [key(4, true)]);
    emit(&h, Port::Keyboard, &[0xf0, 0x1c]);
    assert_eq!(c.poll().unwrap(), [key(4, false)]);
    assert_eq!(c.framing_errors(), 0);
}
