//! Reloj de alta resolución y temporizador para el hilo de grabación.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
    INFINITE, TIMER_ALL_ACCESS,
};

/// Tiempo actual en unidades de 100 ns (mismo reloj que usa WASAPI para sus marcas QPC).
pub fn now_100ns() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    let f = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    });
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    ((c as i128 * 10_000_000) / f as i128) as i64
}

/// Temporizador de alta resolución: duerme el hilo sin girar la CPU.
pub struct Timer(Option<HANDLE>);

impl Timer {
    pub fn new() -> Self {
        let h = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        };
        Timer(h.ok())
    }

    pub fn sleep(&self, hns: i64) {
        if hns <= 0 {
            return;
        }
        if let Some(h) = self.0 {
            let due = -hns;
            unsafe {
                if SetWaitableTimer(h, &due, 0, None, None, false).is_ok() {
                    WaitForSingleObject(h, INFINITE);
                    return;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_nanos(hns as u64 * 100));
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}
