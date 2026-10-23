// The six contract-served families, as a MOCK service: the same generated
// `Server` traits, implemented from the schema alone, answering canned (but
// contract-shaped) data.
//
// The mock is the test double the CLI's six new families are proved against:
// the CLI must present the SERVICE's answers (never its own), and the honest
// empty (a family that serves nothing says so) must survive the seam.

#![allow(dead_code, unused_imports)]

use std::rc::Rc;

use capnp::capability::Results;

use crate::support::mock::Cfg;

/// Copy a built message into the call's results struct (the contract's own
/// results struct, byte for byte).
fn fill<'a, T>(
    message: &'a capnp::message::Builder<capnp::message::HeapAllocator>,
    mut results: Results<T>,
) where
    T: capnp::traits::Owned,
{
    if let Ok(reader) = message.get_root_as_reader::<T::Reader<'a>>() {
        let _ = results.set(reader);
    }
}

/// The read-only class's refusal (the capability class decides).
pub fn denied() -> capnp::Error {
    capnp::Error {
        kind: capnp::ErrorKind::Failed,
        extra: "permissionDenied: a control verb; this connection's capability class is ReadOnly"
            .to_string(),
    }
}

/// A control verb's answer: the capability class decides.
pub fn control<T>(
    cfg: &Cfg,
    results: Results<T>,
) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static
where
    T: capnp::traits::Owned + 'static,
{
    if cfg.control {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<smart_protocol::schema::types_capnp::void_result::Builder<'_>>();
            r.set_success(());
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    } else {
        std::future::ready(Err(denied()))
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `time` family.
// ═════════════════════════════════════════════════════════════════════════

/// The time family: the contract's own `Time` interface.
#[cfg(feature = "schema-time")]
pub struct TimeServer {
    pub cfg: Cfg,
}

#[cfg(feature = "schema-time")]
impl smart_protocol::schema::time_capnp::time::Server for TimeServer {
    fn get_date_time(
        self: Rc<Self>,
        _params: smart_protocol::schema::time_capnp::time::GetDateTimeParams,
        results: smart_protocol::schema::time_capnp::time::GetDateTimeResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::time_capnp::time::get_date_time_results::Builder<'_>>();
            let mut dt = r.reborrow().init_date_time();
            dt.set_year(2026);
            dt.set_month(10);
            dt.set_day(21);
            dt.set_hour(9);
            dt.set_minute(16);
            dt.set_second(4);
            dt.set_millisecond(123);
            dt.set_weekday(3);
            dt.set_yearday(294);
            dt.set_timezone("UTC");
            dt.set_utc_offset_seconds(0);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn convert_calendar(
        self: Rc<Self>,
        params: smart_protocol::schema::time_capnp::time::ConvertCalendarParams,
        results: smart_protocol::schema::time_capnp::time::ConvertCalendarResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let to = params
            .get()
            .ok()
            .and_then(|p| p.get_to_system().ok())
            .unwrap_or(smart_protocol::schema::time_capnp::CalendarSystem::Gregorian);
        // The calendars this double serves; the others are refused with a
        // reason (never approximated).
        let (year, month, day) = match to {
            smart_protocol::schema::time_capnp::CalendarSystem::Julian => (2026, 10, 8),
            smart_protocol::schema::time_capnp::CalendarSystem::Islamic => (1448, 4, 8),
            smart_protocol::schema::time_capnp::CalendarSystem::Ethiopian => (2019, 2, 12),
            _ => (2026, 10, 21),
        };
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::time_capnp::time::convert_calendar_results::Builder<'_>>();
            let mut converted = r.reborrow().init_converted();
            converted.set_year(year);
            converted.set_month(month);
            converted.set_day(day);
            converted.set_calendar(to);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn add_calendar_units(
        self: Rc<Self>,
        _params: smart_protocol::schema::time_capnp::time::AddCalendarUnitsParams,
        results: smart_protocol::schema::time_capnp::time::AddCalendarUnitsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::time_capnp::time::add_calendar_units_results::Builder<'_>>();
            let mut result = r.reborrow().init_result();
            result.set_year(2026);
            result.set_month(11);
            result.set_day(21);
            result.set_calendar(smart_protocol::schema::time_capnp::CalendarSystem::Gregorian);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn subtract_calendar_units(
        self: Rc<Self>,
        _params: smart_protocol::schema::time_capnp::time::SubtractCalendarUnitsParams,
        results: smart_protocol::schema::time_capnp::time::SubtractCalendarUnitsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::time_capnp::time::subtract_calendar_units_results::Builder<'_>>();
            let mut result = r.reborrow().init_result();
            result.set_year(2026);
            result.set_month(9);
            result.set_day(21);
            result.set_calendar(smart_protocol::schema::time_capnp::CalendarSystem::Gregorian);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn diff_calendar_dates(
        self: Rc<Self>,
        _params: smart_protocol::schema::time_capnp::time::DiffCalendarDatesParams,
        results: smart_protocol::schema::time_capnp::time::DiffCalendarDatesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::time_capnp::time::diff_calendar_dates_results::Builder<'_>>();
            let mut diff = r.reborrow().init_diff();
            diff.set_years(1);
            diff.set_months(2);
            diff.set_days(3);
            diff.set_total_days(400);
            diff.set_negative(false);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `logging` family.
// ═════════════════════════════════════════════════════════════════════════

/// The logging family: the contract's own `Logging` interface.
#[cfg(feature = "schema-logging")]
pub struct LoggingServer {
    pub cfg: Cfg,
}

#[cfg(feature = "schema-logging")]
impl smart_protocol::schema::logging_capnp::logging::Server for LoggingServer {
    fn list_domains(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::ListDomainsParams,
        results: smart_protocol::schema::logging_capnp::logging::ListDomainsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::logging_capnp::logging::list_domains_results::Builder<'_>>();
            let mut list = r.reborrow().init_domains(2);
            {
                let mut domain = list.reborrow().get(0);
                domain.set_name("smart.cli");
                domain.set_level(smart_protocol::schema::types_capnp::LogLevel::Info);
                domain.set_description("the CLI's own lines");
                domain.set_sampling_rate(1.0);
            }
            {
                let mut domain = list.reborrow().get(1);
                domain.set_name("smart.service");
                domain.set_level(smart_protocol::schema::types_capnp::LogLevel::Warn);
                domain.set_description("the service's own lines");
                domain.set_sampling_rate(0.5);
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_entries(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::GetEntriesParams,
        results: smart_protocol::schema::logging_capnp::logging::GetEntriesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::logging_capnp::logging::get_entries_results::Builder<'_>>();
            let mut list = r.reborrow().init_entries(2);
            {
                let mut entry = list.reborrow().get(0);
                entry.reborrow().init_id().set_id(1);
                entry.set_sequence(1);
                entry.reborrow().init_timestamp().set_nanoseconds(1_700_000_000_000_000_000);
                entry.set_level(smart_protocol::schema::types_capnp::LogLevel::Info);
                entry.set_domain("smart.cli");
                entry.set_message("the CLI answered a call");
            }
            {
                let mut entry = list.reborrow().get(1);
                entry.reborrow().init_id().set_id(2);
                entry.set_sequence(2);
                entry.reborrow().init_timestamp().set_nanoseconds(1_700_000_000_000_000_001);
                entry.set_level(smart_protocol::schema::types_capnp::LogLevel::Warn);
                entry.set_domain("smart.service");
                entry.set_message("the service answered a call");
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn log(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::LogParams,
        results: smart_protocol::schema::logging_capnp::logging::LogResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r =
                message.init_root::<smart_protocol::schema::logging_capnp::logging::log_results::Builder<'_>>();
            r.reborrow().init_outcome().set_accepted(());
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_level(
        self: Rc<Self>,
        params: smart_protocol::schema::logging_capnp::logging::GetLevelParams,
        results: smart_protocol::schema::logging_capnp::logging::GetLevelResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let domain = params
            .get()
            .ok()
            .and_then(|p| p.get_domain().ok())
            .map(|d| d.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::logging_capnp::logging::get_level_results::Builder<'_>>();
            r.set_level(if domain == "smart.service" {
                smart_protocol::schema::types_capnp::LogLevel::Warn
            } else {
                smart_protocol::schema::types_capnp::LogLevel::Info
            });
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn export(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::ExportParams,
        results: smart_protocol::schema::logging_capnp::logging::ExportResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::logging_capnp::logging::export_results::Builder<'_>>();
            let payload = b"smart.cli info the CLI answered a call\n";
            r.reborrow().init_data().set_inline(payload);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn set_level(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::SetLevelParams,
        results: smart_protocol::schema::logging_capnp::logging::SetLevelResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn set_sampling_rate(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::SetSamplingRateParams,
        results: smart_protocol::schema::logging_capnp::logging::SetSamplingRateResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn clear(
        self: Rc<Self>,
        _params: smart_protocol::schema::logging_capnp::logging::ClearParams,
        results: smart_protocol::schema::logging_capnp::logging::ClearResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `settings` family.
// ═════════════════════════════════════════════════════════════════════════

/// The settings family: the contract's own `Settings` interface.
#[cfg(feature = "schema-settings")]
pub struct SettingsServer {
    pub cfg: Cfg,
}

/// A `SettingValue` the mock serves: a string or a boolean, in the contract's
/// own union.
#[cfg(feature = "schema-settings")]
fn set_string(
    mut value: smart_protocol::schema::settings_capnp::setting_value::Builder<'_>,
    text: &str,
) {
    value.set_string(text);
}

#[cfg(feature = "schema-settings")]
impl smart_protocol::schema::settings_capnp::settings::Server for SettingsServer {
    fn list_domains(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::ListDomainsParams,
        results: smart_protocol::schema::settings_capnp::settings::ListDomainsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::settings_capnp::settings::list_domains_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_domains(2);
            list.set(0, "desktop");
            list.set(1, "network");
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_all(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::GetAllParams,
        results: smart_protocol::schema::settings_capnp::settings::GetAllResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::settings_capnp::settings::get_all_results::Builder<'_>>();
            let mut map = r.reborrow().init_settings();
            let mut entries = map.reborrow().init_entries(2);
            {
                let mut entry = entries.reborrow().get(0);
                entry.set_key("theme");
                set_string(entry.reborrow().init_value(), "dark");
            }
            {
                let mut entry = entries.reborrow().get(1);
                entry.set_key("animations");
                entry.reborrow().init_value().set_boolean(true);
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get(
        self: Rc<Self>,
        params: smart_protocol::schema::settings_capnp::settings::GetParams,
        results: smart_protocol::schema::settings_capnp::settings::GetResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let key = params
            .get()
            .ok()
            .and_then(|p| p.get_key().ok())
            .map(|k| k.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r =
                message.init_root::<smart_protocol::schema::settings_capnp::settings::get_results::Builder<'_>>();
            if key == "theme" {
                set_string(r.reborrow().init_result().init_value(), "dark");
            } else {
                r.reborrow().init_result().set_not_set(());
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_version(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::GetVersionParams,
        results: smart_protocol::schema::settings_capnp::settings::GetVersionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::settings_capnp::settings::get_version_results::Builder<'_>>();
            r.set_version(7);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_history(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::GetHistoryParams,
        results: smart_protocol::schema::settings_capnp::settings::GetHistoryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::settings_capnp::settings::get_history_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_changes(1);
            {
                let mut change = list.reborrow().get(0);
                change.set_domain("desktop");
                change.set_key("theme");
                set_string(change.reborrow().init_value(), "dark");
                change
                    .reborrow()
                    .init_timestamp()
                    .set_nanoseconds(1_700_000_000_000_000_000);
                change.set_actor_id("the user");
                change.set_version(7);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_settings_diff(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::GetSettingsDiffParams,
        results: smart_protocol::schema::settings_capnp::settings::GetSettingsDiffResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::settings_capnp::settings::get_settings_diff_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_changes(1);
            {
                let mut change = list.reborrow().get(0);
                change.set_domain("desktop");
                change.set_key("theme");
                set_string(change.reborrow().init_value(), "dark");
                change.set_version(7);
            }
            r.set_current_version(7);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_by_category(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::ListByCategoryParams,
        results: smart_protocol::schema::settings_capnp::settings::ListByCategoryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::settings_capnp::settings::list_by_category_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_entries(1);
            {
                let mut entry = list.reborrow().get(0);
                entry.set_key("theme");
                set_string(entry.reborrow().init_value(), "dark");
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn bulk_get(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::BulkGetParams,
        results: smart_protocol::schema::settings_capnp::settings::BulkGetResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::settings_capnp::settings::bulk_get_results::Builder<'_>>();
            let mut list = r.reborrow().init_entries(1);
            {
                let mut entry = list.reborrow().get(0);
                entry.set_key("theme");
                set_string(entry.reborrow().init_value(), "dark");
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn set(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::SetParams,
        results: smart_protocol::schema::settings_capnp::settings::SetResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn delete(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::DeleteParams,
        results: smart_protocol::schema::settings_capnp::settings::DeleteResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn clear(
        self: Rc<Self>,
        _params: smart_protocol::schema::settings_capnp::settings::ClearParams,
        results: smart_protocol::schema::settings_capnp::settings::ClearResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `clock` family.
// ═════════════════════════════════════════════════════════════════════════

/// The clock family: the contract's own `ClockService` interface.
#[cfg(feature = "schema-clock")]
pub struct ClockServer {
    pub cfg: Cfg,
}

#[cfg(feature = "schema-clock")]
impl smart_protocol::schema::clock_capnp::clock_service::Server for ClockServer {
    fn info(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::InfoParams,
        results: smart_protocol::schema::clock_capnp::clock_service::InfoResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::clock_capnp::clock_service::info_results::Builder<'_>>();
            let mut info = r.reborrow().init_info();
            info.reborrow().init_monotonic().set_nanoseconds(1_700_000_000_000_000_000);
            info.reborrow().init_realtime().set_nanoseconds(1_700_000_000_000_000_000);
            info.reborrow().init_resolution().set_nanoseconds(1);
            info.set_source_name("systemd-timesyncd");
            info.reborrow()
                .init_boot_time()
                .set_nanoseconds(1_699_000_000_000_000_000);
            {
                let mut sync = info.reborrow().init_sync();
                sync.set_synced(true);
                sync.set_source("ntp.pool.org");
                sync.set_stratum(2);
                sync.reborrow().init_last_sync().set_nanoseconds(1_700_000_000_000_000_000);
                sync.reborrow().init_offset().set_nanoseconds(150_000);
            }
            {
                let mut security = info.reborrow().init_security();
                security.set_tampered(false);
                security.set_secure_source(true);
                security.set_hardware_rng(true);
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn realtime_now(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::RealtimeNowParams,
        results: smart_protocol::schema::clock_capnp::clock_service::RealtimeNowResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::realtime_now_results::Builder<'_>,
            >();
            r.reborrow().init_time().set_nanoseconds(1_700_000_000_000_000_000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn monotonic_now(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::MonotonicNowParams,
        results: smart_protocol::schema::clock_capnp::clock_service::MonotonicNowResults,
    ) -> impl std::future::Future<Output = Result<(), capnp:: Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::monotonic_now_results::Builder<'_>,
            >();
            r.reborrow().init_time().set_nanoseconds(42_000_000_000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_clock_domains(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::ListClockDomainsParams,
        results: smart_protocol::schema::clock_capnp::clock_service::ListClockDomainsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::list_clock_domains_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_domains(2);
            {
                let mut domain = list.reborrow().get(0);
                domain.set_domain(smart_protocol::schema::clock_capnp::ClockDomain::Realtime);
                domain.set_name("CLOCK_REALTIME");
                domain.reborrow().init_resolution().set_nanoseconds(1);
                domain.set_available(true);
            }
            {
                let mut domain = list.reborrow().get(1);
                domain.set_domain(smart_protocol::schema::clock_capnp::ClockDomain::Monotonic);
                domain.set_name("CLOCK_MONOTONIC");
                domain.reborrow().init_resolution().set_nanoseconds(1);
                domain.set_available(true);
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_clock_sync(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::GetClockSyncParams,
        results: smart_protocol::schema::clock_capnp::clock_service::GetClockSyncResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::get_clock_sync_results::Builder<'_>,
            >();
            let mut sync = r.reborrow().init_sync();
            sync.set_synced(true);
            sync.set_source("ntp.pool.org");
            sync.set_stratum(2);
            sync.reborrow().init_last_sync().set_nanoseconds(1_700_000_000_000_000_000);
            sync.reborrow().init_offset().set_nanoseconds(150_000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_clock_quality(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::GetClockQualityParams,
        results: smart_protocol::schema::clock_capnp::clock_service::GetClockQualityResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::get_clock_quality_results::Builder<'_>,
            >();
            let mut quality = r.reborrow().init_quality();
            quality.set_source_name("systemd-timesyncd");
            quality.reborrow().init_accuracy().set_nanoseconds(1_000);
            quality.reborrow().init_stability().set_nanoseconds(50);
            quality.set_reliability(0.99);
            quality.reborrow().init_measured_at().set_nanoseconds(1_700_000_000_000_000_000);
            quality.set_sample_count(128);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_clock_drift(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::GetClockDriftParams,
        results: smart_protocol::schema::clock_capnp::clock_service::GetClockDriftResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::get_clock_drift_results::Builder<'_>,
            >();
            let mut drift = r.reborrow().init_drift();
            drift.reborrow().init_drift().set_nanoseconds(1_500);
            drift.reborrow().init_measured_at().set_nanoseconds(1_700_000_000_000_000_000);
            drift.reborrow().init_accuracy().set_nanoseconds(100);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_persisted_timers(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::ListPersistedTimersParams,
        results: smart_protocol::schema::clock_capnp::clock_service::ListPersistedTimersResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::clock_capnp::clock_service::list_persisted_timers_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_timers(1);
            {
                let mut timer = list.reborrow().get(0);
                timer.reborrow().init_id().set_id(11);
                timer.set_label("the daily sweep");
            }
            r.set_total_count(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn sleep(
        self: Rc<Self>,
        _params: smart_protocol::schema::clock_capnp::clock_service::SleepParams,
        results: smart_protocol::schema::clock_capnp::clock_service::SleepResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::types_capnp::void_result::Builder<'_>,
            >();
            r.set_success(());
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `process` family.
// ═════════════════════════════════════════════════════════════════════════

/// The process family: the contract's own `Process` interface.
#[cfg(feature = "schema-process")]
pub struct ProcessServer {
    pub cfg: Cfg,
}

#[cfg(feature = "schema-process")]
impl smart_protocol::schema::process_capnp::process::Server for ProcessServer {
    fn list_processes(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::ListProcessesParams,
        results: smart_protocol::schema::process_capnp::process::ListProcessesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::list_processes_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_processes(2);
            {
                let mut info = list.reborrow().get(0);
                info.reborrow().init_pid().set_id(1);
                info.reborrow().init_ppid().set_id(0);
                info.set_name("init");
                info.set_command("/sbin/init");
                info.set_state(smart_protocol::schema::types_capnp::ExecutionState::Active);
                info.set_cpu_usage(0.01);
                info.set_memory_usage(2_097_152);
                info.set_threads(1);
                info.reborrow().init_start_time().set_nanoseconds(1_699_000_000_000_000_000);
                info.reborrow().init_user().set_id(0);
            }
            {
                let mut info = list.reborrow().get(1);
                info.reborrow().init_pid().set_id(4242);
                info.reborrow().init_ppid().set_id(1);
                info.set_name("smart-service");
                info.set_command("/usr/bin/smart-service");
                info.set_state(smart_protocol::schema::types_capnp::ExecutionState::Active);
                info.set_cpu_usage(0.25);
                info.set_memory_usage(52_428_800);
                info.set_threads(8);
                info.reborrow().init_start_time().set_nanoseconds(1_699_900_000_000_000_000);
                info.reborrow().init_user().set_id(1000);
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_process(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetProcessParams,
        results: smart_protocol::schema::process_capnp::process::GetProcessResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::process_capnp::process::get_process_results::Builder<'_>>();
            let mut info = r.reborrow().init_info();
            info.reborrow().init_pid().set_id(4242);
            info.reborrow().init_ppid().set_id(1);
            info.set_name("smart-service");
            info.set_command("/usr/bin/smart-service");
            info.set_state(smart_protocol::schema::types_capnp::ExecutionState::Active);
            info.set_cpu_usage(0.25);
            info.set_memory_usage(52_428_800);
            info.set_threads(8);
            info.reborrow().init_start_time().set_nanoseconds(1_699_900_000_000_000_000);
            info.reborrow().init_user().set_id(1000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_process_status(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetProcessStatusParams,
        results: smart_protocol::schema::process_capnp::process::GetProcessStatusResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_process_status_results::Builder<'_>,
            >();
            let mut status = r.reborrow().init_status();
            status.reborrow().init_pid().set_id(4242);
            status.set_name("smart-service");
            status.set_state(smart_protocol::schema::types_capnp::ExecutionState::Active);
            status.reborrow().init_uptime().set_nanoseconds(3_600_000_000_000);
            status.set_memory_bytes(52_428_800);
            status.reborrow().init_cpu_time().set_nanoseconds(42_000_000_000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_threads(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetThreadsParams,
        results: smart_protocol::schema::process_capnp::process::GetThreadsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::process_capnp::process::get_threads_results::Builder<'_>>();
            let mut list = r.reborrow().init_threads(2);
            {
                let mut thread = list.reborrow().get(0);
                thread.reborrow().init_id().set_id(4242);
                thread.set_name("smart-service");
                thread.set_state(smart_protocol::schema::types_capnp::ExecutionState::Active);
                thread.set_cpu_usage(0.1);
            }
            {
                let mut thread = list.reborrow().get(1);
                thread.reborrow().init_id().set_id(4243);
                thread.set_name("smart-io");
                thread.set_state(smart_protocol::schema::types_capnp::ExecutionState::Blocked);
                thread.set_cpu_usage(0.0);
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_environment(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetEnvironmentParams,
        results: smart_protocol::schema::process_capnp::process::GetEnvironmentResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_environment_results::Builder<'_>,
            >();
            let mut map = r.reborrow().init_env();
            let mut entries = map.reborrow().init_entries(2);
            {
                let mut entry = entries.reborrow().get(0);
                entry.set_key("HOME");
                entry.set_value("/home/smart");
            }
            {
                let mut entry = entries.reborrow().get(1);
                entry.set_key("PATH");
                entry.set_value("/usr/bin:/bin");
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_memory_map(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetMemoryMapParams,
        results: smart_protocol::schema::process_capnp::process::GetMemoryMapResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_memory_map_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_regions(1);
            {
                let mut region = list.reborrow().get(0);
                region.set_start(0x1000);
                region.set_end(0x3000);
                region.set_perms(smart_protocol::schema::types_capnp::MemoryPerms::Read);
                region.set_name("/usr/bin/smart-service");
                region.set_backing("/usr/bin/smart-service");
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_resource_limits(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetResourceLimitsParams,
        results: smart_protocol::schema::process_capnp::process::GetResourceLimitsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_resource_limits_results::Builder<'_>,
            >();
            let mut quota = r.reborrow().init_limits();
            quota.set_memory_bytes(536_870_912);
            quota.set_cpu_percent(80.0);
            quota.set_io_read_bytes_per_sec(1_048_576);
            quota.set_io_write_bytes_per_sec(1_048_576);
            quota.set_pids(512);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_accounting(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetAccountingParams,
        results: smart_protocol::schema::process_capnp::process::GetAccountingResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_accounting_results::Builder<'_>,
            >();
            let mut usage = r.reborrow().init_usage();
            usage.reborrow().init_cpu_time().set_nanoseconds(42_000_000_000);
            usage.set_memory_bytes(52_428_800);
            usage.set_peak_memory_bytes(62_914_560);
            usage.set_io_read_bytes(1024);
            usage.set_io_write_bytes(2048);
            usage.set_network_rx_bytes(4096);
            usage.set_network_tx_bytes(8192);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_stats(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetStatsParams,
        results: smart_protocol::schema::process_capnp::process::GetStatsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::process_capnp::process::get_stats_results::Builder<'_>>();
            let mut stats = r.reborrow().init_stats();
            stats.set_total_processes(2);
            stats.set_active(2);
            stats.set_blocked(0);
            stats.set_suspended(0);
            stats.set_terminated(0);
            stats.set_total_cpu_usage(0.26);
            stats.set_total_memory_usage(54_525_952);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_process_groups(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::ListProcessGroupsParams,
        results: smart_protocol::schema::process_capnp::process::ListProcessGroupsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::list_process_groups_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_groups(1);
            {
                let mut group = list.reborrow().get(0);
                group.reborrow().init_group_id().set_id(4242);
                group.set_name("smart-service");
                group.reborrow().init_session_id().set_id(4242);
                group.set_foreground(true);
                let mut members = group.reborrow().init_processes(1);
                members.reborrow().get(0).set_id(4242);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    // The mutating verbs: the capability class decides them.
    fn attach(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::AttachParams,
        results: smart_protocol::schema::process_capnp::process::AttachResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn detach(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::DetachParams,
        results: smart_protocol::schema::process_capnp::process::DetachResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn get_execution_context(
        self: Rc<Self>,
        _params: smart_protocol::schema::process_capnp::process::GetExecutionContextParams,
        results: smart_protocol::schema::process_capnp::process::GetExecutionContextResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::process_capnp::process::get_execution_context_results::Builder<'_>,
            >();
            let mut context = r.reborrow().init_context();
            context.reborrow().init_pid().set_id(4242);
            let mut capabilities = context.reborrow().init_capabilities(1);
            capabilities.reborrow().get(0).set_handle(1);
            {
                let mut quota = context.reborrow().init_quota();
                quota.set_memory_bytes(536_870_912);
                quota.set_cpu_percent(80.0);
                quota.set_pids(512);
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}

// ═════════════════════════════════════════════════════════════════════════
// The `files` family.
// ═════════════════════════════════════════════════════════════════════════

/// The files family: the contract's own `Files` interface.
#[cfg(feature = "schema-files")]
pub struct FilesServer {
    pub cfg: Cfg,
}

#[cfg(feature = "schema-files")]
impl smart_protocol::schema::files_capnp::files::Server for FilesServer {
    fn list_directory(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::ListDirectoryParams,
        results: smart_protocol::schema::files_capnp::files::ListDirectoryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::list_directory_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_entries(2);
            {
                let mut entry = list.reborrow().get(0);
                entry.set_name("README.md");
                entry.set_path("README.md");
                entry.set_type(smart_protocol::schema::files_capnp::FileType::Regular);
                entry.set_size(1024);
                entry.reborrow().init_modified().set_nanoseconds(1_700_000_000_000_000_000);
            }
            {
                let mut entry = list.reborrow().get(1);
                entry.set_name("src");
                entry.set_path("src");
                entry.set_type(smart_protocol::schema::files_capnp::FileType::Directory);
                entry.set_size(4096);
                entry.reborrow().init_modified().set_nanoseconds(1_700_000_000_000_000_000);
            }
            r.set_total(2);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn search_files(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::SearchFilesParams,
        results: smart_protocol::schema::files_capnp::files::SearchFilesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::search_files_results::Builder<'_>>();
            let mut list = r.reborrow().init_results(1);
            {
                let mut row = list.reborrow().get(0);
                row.set_path("README.md");
                row.set_name("README.md");
                row.set_type(smart_protocol::schema::files_capnp::FileType::Regular);
                row.set_size(1024);
                row.reborrow().init_modified().set_nanoseconds(1_700_000_000_000_000_000);
                row.set_score(1.0);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn walk_tree(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::WalkTreeParams,
        results: smart_protocol::schema::files_capnp::files::WalkTreeResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::walk_tree_results::Builder<'_>>();
            let mut list = r.reborrow().init_entries(1);
            {
                let mut entry = list.reborrow().get(0);
                entry.set_name("src");
                entry.set_path("src");
                entry.set_type(smart_protocol::schema::files_capnp::FileType::Directory);
                entry.set_size(4096);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn stat_file(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::StatFileParams,
        results: smart_protocol::schema::files_capnp::files::StatFileResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::stat_file_results::Builder<'_>>();
            let mut result = r.reborrow().init_result();
            let mut info = result.reborrow().init_info();
            info.set_path("README.md");
            info.set_type(smart_protocol::schema::files_capnp::FileType::Regular);
            info.set_size(1024);
            info.reborrow().init_created().set_nanoseconds(1_699_000_000_000_000_000);
            info.reborrow().init_modified().set_nanoseconds(1_700_000_000_000_000_000);
            info.reborrow().init_accessed().set_nanoseconds(1_700_000_000_000_000_000);
            {
                let mut mime = info.reborrow().init_mime_type();
                mime.set_type("text");
                mime.set_subtype("markdown");
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_file_metadata(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetFileMetadataParams,
        results: smart_protocol::schema::files_capnp::files::GetFileMetadataResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_file_metadata_results::Builder<'_>,
            >();
            let mut metadata = r.reborrow().init_metadata();
            metadata.set_path("README.md");
            metadata.set_type(smart_protocol::schema::files_capnp::FileType::Regular);
            metadata.set_size(1024);
            metadata.reborrow().init_created().set_nanoseconds(1_699_000_000_000_000_000);
            metadata.reborrow().init_modified().set_nanoseconds(1_700_000_000_000_000_000);
            metadata.reborrow().init_accessed().set_nanoseconds(1_700_000_000_000_000_000);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_disk_usage(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetDiskUsageParams,
        results: smart_protocol::schema::files_capnp::files::GetDiskUsageResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::get_disk_usage_results::Builder<'_>>();
            let mut usage = r.reborrow().init_usage();
            usage.set_total(100_000_000_000);
            usage.set_used(42_000_000_000);
            usage.set_free(58_000_000_000);
            usage.set_percent_used(42.0);
            usage.set_mount_point("/");
            usage.reborrow().init_mount_id().set_id(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_locks(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetLocksParams,
        results: smart_protocol::schema::files_capnp::files::GetLocksResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::get_locks_results::Builder<'_>>();
            let mut list = r.reborrow().init_locks(1);
            {
                let mut lock = list.reborrow().get(0);
                lock.set_lock_id(7);
                lock.set_lock_type(smart_protocol::schema::files_capnp::FileLockType::Exclusive);
                lock.set_owner("smart-service");
                lock.reborrow().init_acquired_at().set_nanoseconds(1_700_000_000_000_000_000);
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_file_hash(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetFileHashParams,
        results: smart_protocol::schema::files_capnp::files::GetFileHashResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::get_file_hash_results::Builder<'_>>();
            let mut result = r.reborrow().init_result();
            {
                let mut hash = result.reborrow().init_hash();
                hash.set_algorithm(1);
                hash.set_bytes(&[0xde, 0xad, 0xbe, 0xef]);
            }
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_file_versions(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetFileVersionsParams,
        results: smart_protocol::schema::files_capnp::files::GetFileVersionsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_file_versions_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_versions(1);
            {
                let mut version = list.reborrow().get(0);
                version.set_version_id(3);
                version.set_size(1024);
                version.set_message("the first cut");
                version.reborrow().init_created_at().set_nanoseconds(1_699_000_000_000_000_000);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_branches(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::ListBranchesParams,
        results: smart_protocol::schema::files_capnp::files::ListBranchesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::list_branches_results::Builder<'_>>();
            let mut list = r.reborrow().init_branches(1);
            {
                let mut branch = list.reborrow().get(0);
                branch.set_branch_id("main");
                branch.set_branch_name("main");
                branch.set_path("README.md");
                branch.set_base_version(3);
                branch.reborrow().init_created_at().set_nanoseconds(1_699_000_000_000_000_000);
                branch.set_author("the user");
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn list_trash(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::ListTrashParams,
        results: smart_protocol::schema::files_capnp::files::ListTrashResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::list_trash_results::Builder<'_>>();
            let mut list = r.reborrow().init_entries(1);
            {
                let mut entry = list.reborrow().get(0);
                entry.set_entry_id("trash-1");
                entry.set_original_path("old-notes.txt");
                entry.set_size(64);
                entry.reborrow().init_deleted_at().set_nanoseconds(1_699_500_000_000_000_000);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_recent_files(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetRecentFilesParams,
        results: smart_protocol::schema::files_capnp::files::GetRecentFilesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_recent_files_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_files(1);
            {
                let mut file = list.reborrow().get(0);
                file.set_path("README.md");
                {
                    let mut mime = file.reborrow().init_mime_type();
                    mime.set_type("text");
                    mime.set_subtype("markdown");
                }
                file.reborrow().init_timestamp().set_nanoseconds(1_700_000_000_000_000_000);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_default_app(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetDefaultAppParams,
        results: smart_protocol::schema::files_capnp::files::GetDefaultAppResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::get_default_app_results::Builder<'_>>();
            let mut result = r.reborrow().init_result();
            let mut app = result.reborrow().init_app();
            app.set_name("editor");
            app.reborrow().init_app_id().set_id(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn search_file_content(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::SearchFileContentParams,
        results: smart_protocol::schema::files_capnp::files::SearchFileContentResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::search_file_content_results::Builder<'_>,
            >();
            let mut list = r.reborrow().init_results(1);
            {
                let mut row = list.reborrow().get(0);
                row.set_path("README.md");
                row.set_line_number(12);
                row.set_snippet("the spine holds the contract");
                row.set_score(1.0);
            }
            r.set_total(1);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn compare_files(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::CompareFilesParams,
        results: smart_protocol::schema::files_capnp::files::CompareFilesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::compare_files_results::Builder<'_>>();
            r.reborrow().init_result().set_identical(true);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_filesystem_health(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetFilesystemHealthParams,
        results: smart_protocol::schema::files_capnp::files::GetFilesystemHealthResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_filesystem_health_results::Builder<'_>,
            >();
            let mut result = r.reborrow().init_result();
            let mut health = result.reborrow().init_health();
            health.set_status("healthy");
            health.reborrow().init_last_check().set_nanoseconds(1_700_000_000_000_000_000);
            health.reborrow()
                .init_next_scheduled()
                .set_nanoseconds(1_700_003_600_000_000_000);
            health.set_errors(0);
            health.set_warnings(0);
            health.set_details("every mount answers");
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_quota(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetQuotaParams,
        results: smart_protocol::schema::files_capnp::files::GetQuotaResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::get_quota_results::Builder<'_>>();
            let mut quota = r.reborrow().init_quota();
            quota.set_max_size_bytes(1_000_000_000);
            quota.set_max_files(10_000);
            quota.set_current_size_bytes(12_345);
            quota.set_current_files(3);
            quota.set_soft_limit_percent(80);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn file_exists(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::FileExistsParams,
        results: smart_protocol::schema::files_capnp::files::FileExistsResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message
                .init_root::<smart_protocol::schema::files_capnp::files::file_exists_results::Builder<'_>>();
            r.set_exists(true);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    fn get_current_directory(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetCurrentDirectoryParams,
        results: smart_protocol::schema::files_capnp::files::GetCurrentDirectoryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_current_directory_results::Builder<'_>,
            >();
            r.set_path("/home/smart");
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }

    // The mutating verbs: the capability class decides them.
    fn remove_file(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::RemoveFileParams,
        results: smart_protocol::schema::files_capnp::files::RemoveFileResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn create_directory(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::CreateDirectoryParams,
        results: smart_protocol::schema::files_capnp::files::CreateDirectoryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn touch(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::TouchParams,
        results: smart_protocol::schema::files_capnp::files::TouchResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        control(&self.cfg, results)
    }

    fn get_directory_size(
        self: Rc<Self>,
        _params: smart_protocol::schema::files_capnp::files::GetDirectorySizeParams,
        results: smart_protocol::schema::files_capnp::files::GetDirectorySizeResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = &self.cfg;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut r = message.init_root::<
                smart_protocol::schema::files_capnp::files::get_directory_size_results::Builder<'_>,
            >();
            r.set_size(12_345);
        }
        fill(&message, results);
        std::future::ready(Ok(()))
    }
}
