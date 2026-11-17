//! 提醒音量：按应用调 Windows 音量混合器里的会话音量。
//!
//! Windows 没有“只调通知提示音”的官方通道：微信、QQ 的提示音和语音、视频共用
//! 同一个应用音频会话，所以这里调的是整个应用在音量混合器里的音量。默认 100
//! 表示不改动，避免启动时把用户已经调好的应用音量顶回去。
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

static TARGET: AtomicU8 = AtomicU8::new(100);

/// 需要跟随提醒音量一起调整的聊天应用进程名。
const APPS: &[&str] = &["WeChat.exe", "Weixin.exe", "QQ.exe", "TIM.exe"];

pub fn set_target(percent: u8) {
    TARGET.store(percent.min(100), Ordering::Relaxed);
}

/// 启动后台线程：定期把当前目标音量应用到新出现的应用会话。
/// 只有用户把音量调离 100 时才真的改动；回到 100 时最后再恢复一次。
pub fn start(initial: u8) {
    let initial = initial.min(100);
    TARGET.store(initial, Ordering::Relaxed);
    std::thread::spawn(move || {
        let mut applied = 100u8;
        let mut at = Instant::now();
        if initial < 100 {
            apply(initial);
            applied = initial;
            at = Instant::now();
        }
        loop {
            let target = TARGET.load(Ordering::Relaxed).min(100);
            if target != applied || (target < 100 && at.elapsed() >= Duration::from_secs(10)) {
                apply(target);
                applied = target;
                at = Instant::now();
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

fn apply(percent: u8) {
    for app in APPS {
        crate::music_volume::set_volume(app, percent as u32);
    }
}
