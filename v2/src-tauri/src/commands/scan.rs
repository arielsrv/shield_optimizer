//! Network-scan command — matches v1's `Scan-Network` UX.

use std::time::Duration;

use serde::Serialize;
use tauri::State;

use crate::adb::{
    local_subnet_prefix, parse_mdns_services, scan_subnet, AdbDriver, MdnsService, ADB_NETWORK_PORT,
};

use super::AppState;

#[derive(Serialize)]
pub struct ScanResult {
    /// First three octets of the scanned /24 (e.g. "192.168.42"), or `null`
    /// if the gateway couldn't be detected.
    pub subnet: Option<String>,
    /// IPs that answered on the ADB port.
    pub found: Vec<String>,
    /// IPs that `adb connect` succeeded against.
    pub connected: Vec<String>,
    /// IPs the daemon reached but that haven't authorized this computer's ADB
    /// key — the device shows an "Allow USB debugging?" prompt and registers
    /// as `unauthorized` in the device list.
    pub unauthorized: Vec<String>,
    /// IPs that responded to the port probe but `adb connect` failed.
    pub failed: Vec<String>,
    /// Devices advertising only an Android 11+ *pairing* service. These cannot
    /// be connected to until the user enters the 6-digit code from the TV, so
    /// they are reported separately rather than counted as failures. Each
    /// entry is the pairing `host:port` to type into Pair PIN.
    pub needs_pairing: Vec<String>,
    /// Human-readable summary line — useful diagnostic for the UI.
    pub message: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ConnectOutcome {
    Connected,
    /// Device reachable but this host's ADB key isn't approved yet. The
    /// device is still added to `adb devices` (as `unauthorized`), so this
    /// is "waiting on the user", not a failure — and retrying won't help.
    Unauthorized,
    Failed,
}

/// Classify `adb connect <target>` output. Detection is text-based on the
/// combined streams rather than the exit code, since `adb connect` exits 0
/// even on "failed to authenticate" / "failed to connect" with current
/// platform-tools, and exit-code conventions vary across versions.
pub(crate) fn classify_connect_output(combined: &str) -> ConnectOutcome {
    let s = combined.to_lowercase();
    if s.contains("failed to authenticate") {
        ConnectOutcome::Unauthorized
    } else if s.contains("connected to") && !s.contains("failed") && !s.contains("cannot") {
        ConnectOutcome::Connected
    } else {
        ConnectOutcome::Failed
    }
}

/// A nonzero exit surfaces as `Err` from the driver and counts as failed.
async fn adb_connect(adb: &dyn AdbDriver, target: &str) -> ConnectOutcome {
    match adb.raw(&["connect", target]).await {
        Ok(out) => classify_connect_output(&format!("{}\n{}", out.stdout, out.stderr)),
        Err(_) => ConnectOutcome::Failed,
    }
}

/// Where the scan will try to connect, and what still needs pairing first.
struct ScanTargets {
    /// `host:port` endpoints to hand to `adb connect`, in a stable order.
    connect: Vec<String>,
    /// Pairing `host:port` for devices that advertise no connectable service.
    needs_pairing: Vec<String>,
}

/// Merge mDNS advertisements with the raw `:5555` port sweep.
///
/// Android 11+ wireless debugging listens on a random port that changes every
/// time it is toggled, so a `:5555` sweep cannot see it at all — which is what
/// "device not supported" actually meant in GitHub #88. mDNS knows the real
/// port, so when a host advertises one we use it and drop the swept `:5555`
/// guess for that same host. Legacy devices (Shield with Network debugging)
/// advertise `_adb._tcp` on 5555 or nothing at all, and keep working either way.
fn merge_scan_targets(swept_ips: &[String], services: &[MdnsService]) -> ScanTargets {
    let mut connect: Vec<String> = Vec::new();
    let mut advertised_hosts: Vec<&str> = Vec::new();

    for service in services.iter().filter(|s| s.is_connectable()) {
        let endpoint = service.endpoint();
        if !connect.contains(&endpoint) {
            connect.push(endpoint);
        }
        if !advertised_hosts.contains(&service.host.as_str()) {
            advertised_hosts.push(&service.host);
        }
    }

    for ip in swept_ips {
        // A host that told us its port is not worth guessing at.
        if advertised_hosts.contains(&ip.as_str()) {
            continue;
        }
        let endpoint = format!("{ip}:{ADB_NETWORK_PORT}");
        if !connect.contains(&endpoint) {
            connect.push(endpoint);
        }
    }

    // Only report pairing for a device we have no way to reach otherwise;
    // an already-paired TV advertises both services and needs no code.
    let mut needs_pairing: Vec<String> = Vec::new();
    for service in services.iter().filter(|s| s.is_pairing()) {
        if advertised_hosts.contains(&service.host.as_str()) {
            continue;
        }
        let endpoint = service.endpoint();
        if !needs_pairing.contains(&endpoint) {
            needs_pairing.push(endpoint);
        }
    }

    ScanTargets {
        connect,
        needs_pairing,
    }
}

/// Ask the adb daemon what it has seen advertised over mDNS. Requires
/// platform-tools 30+; older binaries print usage text to stderr, which
/// parses to no services rather than an error.
async fn discover_mdns_services(adb: &dyn AdbDriver) -> Vec<MdnsService> {
    match adb.raw(&["mdns", "services"]).await {
        Ok(out) => parse_mdns_services(&out.stdout),
        Err(_) => Vec::new(),
    }
}

/// `scan_network` — sweep the local /24 for ADB-listening devices and try
/// `adb connect` against each responder. Returns a structured summary so the
/// UI can render counts and any per-IP failures.
#[tauri::command]
pub async fn scan_network(state: State<'_, AppState>) -> Result<ScanResult, String> {
    let Some(prefix) = local_subnet_prefix().await else {
        return Ok(ScanResult {
            subnet: None,
            found: vec![],
            connected: vec![],
            unauthorized: vec![],
            failed: vec![],
            needs_pairing: vec![],
            message: "Could not detect default gateway. Set SHIELD_OPTIMIZER_SUBNET=\"a.b.c\" \
                      to override, or use Connect IP."
                .to_string(),
        });
    };
    let subnet_label = format!("{}.{}.{}", prefix[0], prefix[1], prefix[2]);

    let hits = scan_subnet(prefix).await;
    let swept: Vec<String> = hits.iter().map(|h| h.ip.clone()).collect();

    let adb = state.adb_snapshot().await;

    // Warm the adb daemon before connecting. The port sweep just opened and
    // dropped raw TCP sockets against each device's adbd; firing `adb connect`
    // immediately afterward — especially against a cold daemon — tends to get
    // a transient refusal, which is why a manual "Restart ADB" (which starts
    // the daemon) made the same devices connect. Starting the server here, plus
    // a single retry below, makes the scan connect on its own.
    let _ = adb.raw(&["start-server"]).await;

    // The daemon has to be up before it can report what it has browsed.
    let services = discover_mdns_services(adb.as_ref()).await;
    let targets = merge_scan_targets(&swept, &services);

    let found: Vec<String> = targets
        .connect
        .iter()
        .cloned()
        .chain(targets.needs_pairing.iter().cloned())
        .collect();

    let mut connected = Vec::new();
    let mut unauthorized = Vec::new();
    let mut failed = Vec::new();
    for target in &targets.connect {
        let mut outcome = adb_connect(adb.as_ref(), target).await;
        // Only a hard failure is worth retrying — "unauthorized" means the
        // device is waiting for the user to approve the prompt on-screen.
        if outcome == ConnectOutcome::Failed {
            tokio::time::sleep(Duration::from_millis(400)).await;
            outcome = adb_connect(adb.as_ref(), target).await;
        }
        match outcome {
            ConnectOutcome::Connected => connected.push(target.clone()),
            ConnectOutcome::Unauthorized => unauthorized.push(target.clone()),
            ConnectOutcome::Failed => failed.push(target.clone()),
        }
    }

    let message = summary_message(
        &subnet_label,
        found.len(),
        &connected,
        &unauthorized,
        &targets.needs_pairing,
    );

    Ok(ScanResult {
        subnet: Some(subnet_label),
        found,
        connected,
        unauthorized,
        failed,
        needs_pairing: targets.needs_pairing,
        message,
    })
}

fn summary_message(
    subnet_label: &str,
    found: usize,
    connected: &[String],
    unauthorized: &[String],
    needs_pairing: &[String],
) -> String {
    if found == 0 {
        return format!(
            "No devices on {subnet_label}.x answered on the ADB port. Make sure Network \
             Debugging is enabled on your TV, or use Connect IP for newer Google TVs that \
             need PIN pairing first."
        );
    }
    let mut message = format!(
        "Scanned {subnet_label}.x — found {found} device{}, connected {}.",
        if found == 1 { "" } else { "s" },
        connected.len()
    );
    if !unauthorized.is_empty() {
        message.push_str(&format!(
            " {} need{} authorization — accept the \"Allow USB debugging?\" prompt on the \
             TV, then Refresh.",
            unauthorized.len(),
            if unauthorized.len() == 1 { "s" } else { "" }
        ));
    }
    if !needs_pairing.is_empty() {
        message.push_str(&format!(
            " {} waiting to be paired — on the TV open Wireless debugging → Pair device \
             with pairing code, then use Pair PIN with {}.",
            needs_pairing.len(),
            needs_pairing.join(", ")
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn service(instance: &str, kind: &str, host: &str, port: u16) -> MdnsService {
        MdnsService {
            instance: instance.into(),
            service: kind.into(),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn an_advertised_port_is_used_instead_of_guessing_5555() {
        // The #88 device: Android 14, wireless debugging on a random port.
        // A :5555 sweep never sees it, and connecting to :5555 never works.
        let services = vec![service(
            "adb-58040DLCH005YV-jBeCEe",
            crate::adb::MDNS_SERVICE_CONNECT,
            "192.168.42.211",
            41541,
        )];

        let targets = merge_scan_targets(&[], &services);

        assert_eq!(targets.connect, vec!["192.168.42.211:41541"]);
        assert!(targets.needs_pairing.is_empty());
    }

    #[test]
    fn an_advertised_host_is_not_also_probed_on_5555() {
        // The sweep can still see the host (some devices listen on both), but
        // the advertised port is the one that is actually current.
        let services = vec![service(
            "adb-tv",
            crate::adb::MDNS_SERVICE_CONNECT,
            "192.168.42.211",
            41541,
        )];
        let swept = vec!["192.168.42.211".to_string(), "192.168.42.71".to_string()];

        let targets = merge_scan_targets(&swept, &services);

        assert_eq!(
            targets.connect,
            vec!["192.168.42.211:41541", "192.168.42.71:5555"]
        );
        assert!(!targets.connect.iter().any(|t| t == "192.168.42.211:5555"));
    }

    #[test]
    fn legacy_devices_still_reach_5555_when_nothing_is_advertised() {
        // The Shield path, unchanged: no mDNS, swept on the standard port.
        let targets = merge_scan_targets(&["192.168.42.71".to_string()], &[]);

        assert_eq!(targets.connect, vec!["192.168.42.71:5555"]);
        assert!(targets.needs_pairing.is_empty());
    }

    #[test]
    fn a_pairing_only_device_is_reported_rather_than_connected_to() {
        // Connecting to the pairing port always fails, and reporting it as a
        // failure tells the user nothing. It needs a code from the TV.
        let services = vec![service(
            "adb-tcl",
            crate::adb::MDNS_SERVICE_PAIRING,
            "192.168.42.211",
            37199,
        )];

        let targets = merge_scan_targets(&[], &services);

        assert!(targets.connect.is_empty());
        assert_eq!(targets.needs_pairing, vec!["192.168.42.211:37199"]);
    }

    #[test]
    fn an_already_paired_device_is_not_asked_to_pair_again() {
        // A paired TV advertises both services; only the connect one matters.
        let services = vec![
            service(
                "adb-tcl-pair",
                crate::adb::MDNS_SERVICE_PAIRING,
                "192.168.42.211",
                37199,
            ),
            service(
                "adb-tcl-connect",
                crate::adb::MDNS_SERVICE_CONNECT,
                "192.168.42.211",
                41541,
            ),
        ];

        let targets = merge_scan_targets(&[], &services);

        assert_eq!(targets.connect, vec!["192.168.42.211:41541"]);
        assert!(targets.needs_pairing.is_empty());
    }

    #[test]
    fn repeated_advertisements_produce_one_target_each() {
        let services = vec![
            service("a", crate::adb::MDNS_SERVICE_LEGACY, "192.168.42.71", 5555),
            service("a", crate::adb::MDNS_SERVICE_LEGACY, "192.168.42.71", 5555),
        ];

        let targets = merge_scan_targets(&["192.168.42.71".to_string()], &services);

        assert_eq!(targets.connect, vec!["192.168.42.71:5555"]);
    }

    #[test]
    fn summary_tells_the_user_how_to_pair_a_waiting_device() {
        let message = summary_message(
            "192.168.42",
            1,
            &[],
            &[],
            &["192.168.42.211:37199".to_string()],
        );

        assert!(message.contains("1 waiting to be paired"));
        assert!(message.contains("192.168.42.211:37199"));
        assert!(message.contains("Pair PIN"));
    }

    #[test]
    fn summary_says_nothing_about_pairing_when_nothing_is_waiting() {
        let message = summary_message(
            "192.168.42",
            1,
            &["192.168.42.71:5555".to_string()],
            &[],
            &[],
        );

        assert!(!message.contains("paired"));
    }

    #[test]
    fn classifies_fresh_and_already_connected() {
        assert_eq!(
            classify_connect_output("connected to 192.168.42.71:5555"),
            ConnectOutcome::Connected
        );
        assert_eq!(
            classify_connect_output("already connected to 192.168.42.71:5555"),
            ConnectOutcome::Connected
        );
    }

    #[test]
    fn classifies_connected_despite_daemon_startup_noise() {
        let out = "* daemon not running; starting now at tcp:5037\n\
                   * daemon started successfully\nconnected to 192.168.42.71:5555";
        assert_eq!(classify_connect_output(out), ConnectOutcome::Connected);
    }

    #[test]
    fn classifies_unauthorized() {
        // Real output from platform-tools 37.0.0 against a TV that hasn't
        // approved this host's key — exits 0, device lands in `adb devices`
        // as `unauthorized`.
        assert_eq!(
            classify_connect_output("failed to authenticate to 192.168.42.143:5555"),
            ConnectOutcome::Unauthorized
        );
    }

    #[test]
    fn classifies_failures() {
        assert_eq!(
            classify_connect_output("failed to connect to '192.168.42.9:5555': Connection refused"),
            ConnectOutcome::Failed
        );
        assert_eq!(
            classify_connect_output("cannot connect to 192.168.42.9:5555: timeout"),
            ConnectOutcome::Failed
        );
        assert_eq!(classify_connect_output(""), ConnectOutcome::Failed);
    }

    #[test]
    fn summary_mentions_unauthorized_devices() {
        let msg = summary_message(
            "192.168.42",
            4,
            &[],
            &["192.168.42.143".into(), "192.168.42.25".into()],
            &[],
        );
        assert_eq!(
            msg,
            "Scanned 192.168.42.x — found 4 devices, connected 0. 2 need authorization — \
             accept the \"Allow USB debugging?\" prompt on the TV, then Refresh."
        );
    }

    #[test]
    fn summary_plain_when_all_connected() {
        let msg = summary_message("10.0.0", 1, &["10.0.0.5".into()], &[], &[]);
        assert_eq!(msg, "Scanned 10.0.0.x — found 1 device, connected 1.");
    }
}
