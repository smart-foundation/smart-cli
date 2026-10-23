use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use smart_protocol::schema::clipboard_capnp as wire;
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

const STEM: &str = "clipboard";
const INTERFACE: &str = "Clipboard";

pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != smart_protocol::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "getContent" => get,
        "setContent" => set,
        "clearContent" => clear,
        "getHistory" => history,
        "clearHistory" => clear_history,
        "getSupportedMimeTypes" => types,
        "searchHistory" => search,
        "pinItem" => pin,
        "unpinItem" => unpin,
        "deleteItem" => delete,
        "getPreview" => preview,
        "validateContent" => validate,
        "convertContent" => convert,
        "getSizeLimits" => limits,
        "setSizeLimits" => set_limits,
        "setOffer" => offer,
        "subscribeChanges" => watch,
        "pull" => pull,
        "cancel" => cancel,
        _ => return None,
    })
}

fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

fn text(r: capnp::Result<capnp::text::Reader<'_>>) -> String {
    r.map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default()
}

fn print_answer(app: &App, json_text: String, human_text: String) -> i32 {
    if app.json() {
        println!("{json_text}");
    } else if !app.quiet() {
        if human_text.ends_with('\n') {
            print!("{human_text}");
        } else {
            println!("{human_text}");
        }
    }
    crate::registry::EXIT_OK
}

fn id_of(args: &[String]) -> u64 {
    domain::positional_number::<u64>(args, 0, 0)
}

fn void_call(app: &mut App, method: &str, params: Vec<u8>) -> i32 {
    let bytes = match super::call_in(app, STEM, INTERFACE, method, &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let json_text = format!("{{\"verb\":\"{}\"}}", method);
    let human_text = format!("  ✎ {} — done", method);
    if app.json() {
        println!("{json_text}");
    } else if !app.quiet() {
        println!("{human_text}");
    }
    let _ = bytes;
    crate::registry::EXIT_OK
}

// getContent
fn get(app: &mut App, args: &[String], path: &str) -> i32 {
    let mime = domain::flag(args, "--mime").unwrap_or_else(|| "text/plain".to_string());
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::get_content_params::Builder<'_>>();
        {
            let mut mt = p.reborrow().init_mime_type();
            let parts: Vec<&str> = mime.splitn(2, '/').collect();
            mt.set_type(parts.get(0).copied().unwrap_or("text"));
            mt.set_subtype(parts.get(1).copied().unwrap_or("plain"));
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getContent", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::get_content_results::Owned, _, _>(&bytes, |r| {
        match r.get_result().ok().and_then(|cr| cr.which().ok()) {
            Some(wire::content_result::Which::Content(c)) => {
                let content = c.ok();
                let mt = content.as_ref().and_then(|cc| cc.get_mime_type().ok());
                let mime = format!("{}/{}", mt.as_ref().and_then(|m|m.get_type().ok()).map(|t|t.to_str().unwrap_or("")).unwrap_or(""), mt.as_ref().and_then(|m|m.get_subtype().ok()).map(|t|t.to_str().unwrap_or("")).unwrap_or(""));
                let data = content.as_ref().and_then(|cc| cc.get_data().ok()).and_then(|h| match h.which() {
                    Ok(smart_protocol::schema::types_capnp::data_handle::Which::Inline(b)) => b.map(|x| x.to_vec()).ok(),
                    _ => None,
                }).unwrap_or_default();
                let txt = String::from_utf8_lossy(&data).to_string();
                (format!("{{\"mime_type\":\"{}\",\"size_bytes\":{},\"text\":\"{}\"}}", mime, data.len(), txt.replace('\\', "\\\\").replace('"', "\\\"")), format!("  📋 Clipboard — {}\n  ├─ mime {}\n  └─ {} bytes", if path.contains("status") { "status" } else { "content" }, mime, data.len()))
            }
            Some(wire::content_result::Which::Empty(_)) => (String::from("{\"empty\":true}"), "  📋 Clipboard — ○ empty".to_string()),
            _ => (String::from("{\"error\":true}"), "  📋 Clipboard — error".to_string()),
        }
    }) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn set(app: &mut App, args: &[String], _path: &str) -> i32 {
    let text_arg = domain::positionals(args).first().map(|s| s.to_string()).unwrap_or_default();
    let mime = domain::flag(args, "--mime").unwrap_or_else(|| "text/plain".to_string());
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::set_content_params::Builder<'_>>();
        let mut c = p.reborrow().init_content();
        {
            let mut mt = c.reborrow().init_mime_type();
            let parts: Vec<&str> = mime.splitn(2, '/').collect();
            mt.set_type(parts.get(0).copied().unwrap_or("text"));
            mt.set_subtype(parts.get(1).copied().unwrap_or("plain"));
        }
        c.reborrow().init_data().set_inline(text_arg.as_bytes());
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "setContent", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "setContent", bytes)
}

fn clear(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| { let _ = m.init_root::<wire::clipboard::clear_content_params::Builder<'_>>(); });
    let bytes = match super::call_in(app, STEM, INTERFACE, "clearContent", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "clearContent", bytes)
}

fn history(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::get_history_params::Builder<'_>>();
        p.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getHistory", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::get_history_results::Owned, _, _>(&bytes, |r| {
        let items: Vec<_> = r.get_items().map(|l| l.iter().collect()).unwrap_or_default();
        let total = r.get_total();
        (format!("{{\"total\":{}}}", total), format!("  📋 Copy History — {} item(s)", items.len()))
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn clear_history(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| { let _ = m.init_root::<wire::clipboard::clear_history_params::Builder<'_>>(); });
    let bytes = match super::call_in(app, STEM, INTERFACE, "clearHistory", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "clearHistory", bytes)
}

fn types(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| { let _ = m.init_root::<wire::clipboard::get_supported_mime_types_params::Builder<'_>>(); });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getSupportedMimeTypes", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::get_supported_mime_types_results::Owned, _, _>(&bytes, |r| {
        let mimes: Vec<String> = r.get_mime_types().map(|l| l.iter().map(|m| format!("{}/{}", m.get_type().ok().map(|t|t.to_str().unwrap_or("")).unwrap_or(""), m.get_subtype().ok().map(|t|t.to_str().unwrap_or("")).unwrap_or(""))).collect()).unwrap_or_default();
        let mut out = String::from("{\"mimeTypes\":[");
        for (i, m) in mimes.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&format!("\"{}\"", m));
        }
        out.push_str("]}");
        (out, format!("  📋 {} MIME type(s)", mimes.len()))
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn search(app: &mut App, args: &[String], _path: &str) -> i32 {
    let query = domain::positionals(args).first().map(|s| s.to_string()).unwrap_or_default();
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::search_history_params::Builder<'_>>();
        p.set_query(&query);
        p.reborrow().init_pagination().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "searchHistory", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::search_history_results::Owned, _, _>(&bytes, |r| {
        let total = r.get_total();
        (format!("{{\"total\":{}}}", total), format!("  📋 Search — done"))
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn pin(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::pin_item_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "pinItem", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "pinItem", bytes)
}

fn unpin(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::unpin_item_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "unpinItem", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "unpinItem", bytes)
}

fn delete(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::delete_item_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "deleteItem", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    void_call(app, "deleteItem", bytes)
}

fn preview(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::get_preview_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getPreview", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::get_preview_results::Owned, _, _>(&bytes, |r| {
        match r.get_result().ok().and_then(|pr| pr.which().ok()) {
            Some(wire::preview_result::Which::Preview(p)) => {
                let pr = p.ok();
                let tp = pr.as_ref().and_then(|x| x.get_text_preview().ok()).map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default();
                let _avail = pr.as_ref().map(|x| x.get_available()).unwrap_or(false);
                let _size = pr.as_ref().map(|x| x.get_original_size()).unwrap_or(0);
                (format!("{{\"textPreview\":\"{}\",\"available\":{},\"originalSize\":{}}}", tp.replace('"', "\\\""), _avail, _size), format!("  📋 Preview — {}", tp))
            }
            _ => (String::from("{\"available\":false}"), "  📋 Preview — unavailable".to_string()),
        }
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn validate(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::validate_content_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "validateContent", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::validate_content_results::Owned, _, _>(&bytes, |r| {
        match r.get_result().ok().and_then(|cv| cv.which().ok()) {
            Some(wire::content_validation::Which::Valid(_)) => (String::from("{\"valid\":true}"), "  ✓ valid".to_string()),
            Some(wire::content_validation::Which::Invalid(e)) => (String::from("{\"valid\":false}"), format!("  ✗ invalid — {}", e.ok().and_then(|x| x.get_message().ok()).map(|t| t.to_str().unwrap_or("").to_string()).unwrap_or_default())),
            _ => (String::from("{\"valid\":null}"), "  ? unknown".to_string()),
        }
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn convert(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = id_of(args);
    let mime = domain::flag(args, "--mime").or_else(|| domain::positionals(args).get(1).map(|s| s.to_string())).unwrap_or_default();
    let params = msg(|m| {
        let mut p = m.init_root::<wire::clipboard::convert_content_params::Builder<'_>>();
        p.reborrow().init_item_id().set_id(id);
        {
            let mut mt = p.reborrow().init_target_mime_type();
            let parts: Vec<&str> = mime.splitn(2, '/').collect();
            mt.set_type(parts.get(0).copied().unwrap_or("text"));
            mt.set_subtype(parts.get(1).copied().unwrap_or("plain"));
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "convertContent", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::convert_content_results::Owned, _, _>(&bytes, |r| {
        match r.get_result().ok().and_then(|cr| cr.which().ok()) {
            Some(wire::conversion_result::Which::Content(c)) => {
                let content = c.ok();
                let mt = content.as_ref().and_then(|cc| cc.get_mime_type().ok());
                let m = format!("{}/{}", mt.as_ref().and_then(|x|x.get_type().ok()).map(|t|t.to_str().unwrap_or("")).unwrap_or(""), mt.as_ref().and_then(|x|x.get_subtype().ok()).map(|t|t.to_str().unwrap_or("")).unwrap_or(""));
                (format!("{{\"mime_type\":\"{}\"}}", m), format!("  📋 Converted — {}", m))
            }
            Some(wire::conversion_result::Which::Unsupported(_)) => (String::from("{\"unsupported\":true}"), "  ○ unsupported".to_string()),
            _ => (String::from("{\"error\":true}"), "  ✗ error".to_string()),
        }
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn limits(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| { let _ = m.init_root::<wire::clipboard::get_size_limits_params::Builder<'_>>(); });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getSizeLimits", &params) {
        Ok(b) => b,
        Err(e) => return super::emit_error(app, &e),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::clipboard::get_size_limits_results::Owned, _, _>(&bytes, |r| {
        let l = r.get_limits().ok();
        let (a, b, c) = (l.as_ref().map(|x| x.get_max_item_size()).unwrap_or(0), l.as_ref().map(|x| x.get_max_history_size()).unwrap_or(0), l.as_ref().map(|x| x.get_max_history_count()).unwrap_or(0));
        (format!("{{\"maxItemSize\":{},\"maxHistorySize\":{},\"maxHistoryCount\":{}}}", a, b, c), format!("  📐 Limits — {} {} {}", a, b, c))
    }) else {
        super::usage_error_app(app, "unreadable");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn set_limits(app: &mut App, _args: &[String], _path: &str) -> i32 { void_call(app, "setSizeLimits", msg(|m| { let _ = m.init_root::<wire::clipboard::set_size_limits_params::Builder<'_>>(); })) }
fn offer(app: &mut App, _args: &[String], _path: &str) -> i32 { void_call(app, "setOffer", msg(|m| { let _ = m.init_root::<wire::clipboard::set_offer_params::Builder<'_>>(); })) }
fn watch(app: &mut App, _args: &[String], _path: &str) -> i32 { void_call(app, "subscribeChanges", msg(|m| { let _ = m.init_root::<wire::clipboard::subscribe_changes_params::Builder<'_>>(); })) }
fn pull(app: &mut App, _args: &[String], _path: &str) -> i32 { void_call(app, "pull", msg(|m| { let _ = m.init_root::<wire::clipboard_change_subscription::pull_params::Builder<'_>>(); })) }
fn cancel(app: &mut App, _args: &[String], _path: &str) -> i32 { void_call(app, "cancel", msg(|m| { let _ = m.init_root::<wire::clipboard_change_subscription::cancel_params::Builder<'_>>(); })) }
