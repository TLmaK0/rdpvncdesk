//! `rdp://` deep links: parsing, OS registration, and (on macOS) receiving them.
//!
//! Accepted forms:
//! - `rdp://host`, `rdp://host:port`, `rdp://user@host:port`, `rdp://DOMAIN%5Cuser@host`
//! - Microsoft's form: `rdp://full%20address=s:host:3389&username=s:DOMAIN\user&desktopwidth=i:1920`
//!
//! Passwords in URLs are ignored on purpose: a link from a web page must never carry or
//! trigger sending credentials to a host the user has not saved.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkTarget {
    pub address: String,
    pub username: String,
    pub domain: String,
    pub width: Option<u16>,
    pub height: Option<u16>,
}

pub fn is_link(arg: &str) -> bool {
    arg.get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("rdp://"))
}

pub fn parse(url: &str) -> Option<LinkTarget> {
    if !is_link(url) {
        return None;
    }
    let rest = url[6..].trim_end_matches('/');
    if rest.is_empty() {
        return None;
    }
    let mut target = if rest.contains("=s:") || rest.contains("=i:") {
        parse_ms_form(rest)?
    } else {
        parse_simple_form(rest)?
    };
    // DOMAIN\user -> domain + user
    if let Some((domain, user)) = target.username.split_once('\\') {
        if target.domain.is_empty() {
            target.domain = domain.to_owned();
        }
        target.username = user.to_owned();
    }
    if target.address.is_empty()
        || target
            .address
            .chars()
            .any(|c| c.is_whitespace() || c == '/')
    {
        return None;
    }
    Some(target)
}

fn parse_simple_form(rest: &str) -> Option<LinkTarget> {
    let rest = rest.split(['?', '#']).next()?;
    let (userinfo, host) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    let username = userinfo
        .map(|u| u.split(':').next().unwrap_or_default()) // drop any password
        .map(percent_decode)
        .unwrap_or_default();
    Some(LinkTarget {
        address: percent_decode(host),
        username,
        ..Default::default()
    })
}

fn parse_ms_form(rest: &str) -> Option<LinkTarget> {
    let mut target = LinkTarget::default();
    for pair in rest.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = percent_decode(key).to_ascii_lowercase();
        let value = percent_decode(value);
        let (kind, value) = value.split_once(':').unwrap_or(("", value.as_str()));
        match (key.as_str(), kind) {
            ("full address", "s") => target.address = value.to_owned(),
            ("username", "s") => target.username = value.to_owned(),
            ("domain", "s") => target.domain = value.to_owned(),
            ("desktopwidth", "i") => target.width = value.parse().ok(),
            ("desktopheight", "i") => target.height = value.parse().ok(),
            _ => {}
        }
    }
    Some(target)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Comparable form of an address: lowercase, without the default port.
pub fn normalize_address(address: &str) -> String {
    let a = address.trim().to_ascii_lowercase();
    a.strip_suffix(":3389").map(str::to_owned).unwrap_or(a)
}

// ---------------------------------------------------------------------------
// Registration as the system handler for rdp://
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn register() -> anyhow::Result<()> {
    use anyhow::Context as _;
    let exe = std::env::current_exe()?.display().to_string();
    let command = format!("\"{exe}\" \"%1\"");
    let base = r"HKCU\Software\Classes\rdp";
    let entries: [(&str, &str, &str); 3] = [
        (base, "", "URL:Remote Desktop Protocol"),
        (base, "URL Protocol", ""),
        (
            r"HKCU\Software\Classes\rdp\shell\open\command",
            "",
            &command,
        ),
    ];
    for (key, name, value) in entries {
        let mut cmd = std::process::Command::new("reg");
        cmd.args(["add", key, "/f", "/t", "REG_SZ", "/d", value]);
        if name.is_empty() {
            cmd.arg("/ve");
        } else {
            cmd.args(["/v", name]);
        }
        let status = cmd.status().context("running reg.exe")?;
        anyhow::ensure!(status.success(), "reg.exe failed for {key}");
    }
    println!("rdp:// links now open with {exe}");
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn register() -> anyhow::Result<()> {
    use anyhow::Context as _;
    let exe = std::env::current_exe()?;
    let dir = directories::BaseDirs::new()
        .context("no home directory")?
        .data_local_dir()
        .join("applications");
    std::fs::create_dir_all(&dir)?;
    let desktop = dir.join("rdpvncdesk.desktop");
    std::fs::write(
        &desktop,
        format!(
            "[Desktop Entry]\nType=Application\nName=rdpvncdesk\nComment=Remote desktop client\n\
             Exec=\"{}\" %u\nTerminal=false\nCategories=Network;RemoteAccess;\n\
             MimeType=x-scheme-handler/rdp;\n",
            exe.display()
        ),
    )
    .with_context(|| format!("writing {}", desktop.display()))?;
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .status();
    let status = std::process::Command::new("xdg-mime")
        .args(["default", "rdpvncdesk.desktop", "x-scheme-handler/rdp"])
        .status()
        .context("running xdg-mime (install xdg-utils)")?;
    anyhow::ensure!(status.success(), "xdg-mime failed");
    println!("rdp:// links now open with {}", exe.display());
    Ok(())
}

#[cfg(target_os = "macos")]
pub use macos::register;

#[cfg(target_os = "macos")]
pub use macos::install_url_handler;

#[cfg(target_os = "macos")]
mod macos {
    //! macOS delivers URLs to a running .app as a `GURL` Apple Event, not as argv.

    use std::sync::Mutex;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread as _, ClassType as _, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    pub const BUNDLE_ID: &str = "io.github.tlmak0.rdpvncdesk";

    const fn fourcc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }
    const K_INTERNET_EVENT_CLASS: u32 = fourcc(b"GURL");
    const K_AE_GET_URL: u32 = fourcc(b"GURL");
    const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");

    type Callback = Box<dyn Fn(String) + Send>;
    static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "RdpvncdeskUrlHandler"]
        struct UrlHandler;

        impl UrlHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
                let desc: Option<Retained<AnyObject>> =
                    unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
                let Some(desc) = desc else { return };
                let url: Option<Retained<NSString>> = unsafe { msg_send![&desc, stringValue] };
                let Some(url) = url else { return };
                if let Ok(cb) = CALLBACK.lock()
                    && let Some(cb) = cb.as_ref()
                {
                    cb(url.to_string());
                }
            }
        }
    );

    /// Starts receiving `rdp://` URLs; `callback` is called on the main thread for each one.
    /// Safe to call more than once: the last callback wins.
    pub fn install_url_handler(callback: impl Fn(String) + Send + 'static) {
        if let Ok(mut slot) = CALLBACK.lock() {
            *slot = Some(Box::new(callback));
        }
        let handler: Retained<UrlHandler> = unsafe { msg_send![UrlHandler::alloc(), init] };
        unsafe {
            let manager: Retained<AnyObject> =
                msg_send![objc2::class!(NSAppleEventManager), sharedAppleEventManager];
            let _: () = msg_send![
                &manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: K_INTERNET_EVENT_CLASS,
                andEventID: K_AE_GET_URL
            ];
        }
        // The event manager does not retain its handler: keep it alive for the process lifetime.
        std::mem::forget(handler);
        let _ = UrlHandler::class();
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultHandlerForURLScheme(
            scheme: *const AnyObject,
            bundle_id: *const AnyObject,
        ) -> i32;
    }

    /// Makes the installed rdpvncdesk.app the default handler for rdp://.
    pub fn register() -> anyhow::Result<()> {
        let scheme = NSString::from_str("rdp");
        let bundle = NSString::from_str(BUNDLE_ID);
        // NSString is toll-free bridged with CFString.
        let status = unsafe {
            LSSetDefaultHandlerForURLScheme(
                (&*scheme as *const NSString).cast(),
                (&*bundle as *const NSString).cast(),
            )
        };
        anyhow::ensure!(
            status == 0,
            "LaunchServices error {status}. Build the app bundle first (packaging/macos/make-app.sh) \
             and open it once so macOS knows about it"
        );
        println!("rdp:// links now open with rdpvncdesk.app");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_forms() {
        assert_eq!(parse("rdp://10.0.0.5").unwrap().address, "10.0.0.5");
        assert_eq!(parse("RDP://srv:3390/").unwrap().address, "srv:3390");
        let t = parse("rdp://CORP%5Calice:secret@srv.example.com:3390").unwrap();
        assert_eq!(t.address, "srv.example.com:3390");
        assert_eq!(t.username, "alice");
        assert_eq!(t.domain, "CORP");
    }

    #[test]
    fn microsoft_form() {
        let t = parse(
            "rdp://full%20address=s:mypc.corp:3389&username=s:CORP%5Cbob&desktopwidth=i:1920&desktopheight=i:1080&audiomode=i:2",
        )
        .unwrap();
        assert_eq!(t.address, "mypc.corp:3389");
        assert_eq!(t.username, "bob");
        assert_eq!(t.domain, "CORP");
        assert_eq!((t.width, t.height), (Some(1920), Some(1080)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("http://x").is_none());
        assert!(parse("rdp://").is_none());
        assert!(parse("rdp://username=s:bob").is_none());
        assert!(parse("rdp://a b").is_none());
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize_address("SRV:3389"), "srv");
        assert_eq!(normalize_address("srv:3390"), "srv:3390");
    }
}
