#!/usr/bin/env bash
# Boot an Android TV emulator to stand in for a physical TV during local testing.
# See README.md in this directory for what the emulator can and cannot prove.
#
#   ./tv-emulator.sh install   # SDK packages (emulator + system image, ~9 GB)
#   ./tv-emulator.sh create    # create the AVD
#   ./tv-emulator.sh start     # boot it and wait for sys.boot_completed
#   ./tv-emulator.sh props     # dump the properties detect_device_type() reads
#   ./tv-emulator.sh catalog   # which app-list packages this image actually has
#   ./tv-emulator.sh expose    # publish the guest's adbd on a host TCP port
#   ./tv-emulator.sh stop
#   ./tv-emulator.sh destroy
#
# Env overrides: ATV_EMU_API (34)  ATV_EMU_TAG (android-tv)  ATV_EMU_ABI (arm64-v8a)
#                ATV_EMU_NAME      ATV_EMU_PORT (5580, host port used by `expose`)
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/sdk}"
API="${ATV_EMU_API:-34}"
TAG="${ATV_EMU_TAG:-android-tv}"
ABI="${ATV_EMU_ABI:-arm64-v8a}"
NAME="${ATV_EMU_NAME:-atv-${TAG}-${API}}"
HOST_PORT="${ATV_EMU_PORT:-5580}"
IMAGE="system-images;android-${API};${TAG};${ABI}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
AVDMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager"
EMULATOR="$ANDROID_HOME/emulator/emulator"
ADB="$ANDROID_HOME/platform-tools/adb"
[ -x "$ADB" ] || ADB="$(command -v adb)"

die() { echo "error: $*" >&2; exit 1; }

# The emulator's adb serial, e.g. emulator-5554. Matches while still offline
# (booting), so callers must wait for sys.boot_completed themselves.
serial() {
  "$ADB" devices | awk '/^emulator-[0-9]+\t/ {print $1; exit}'
}

require_running() {
  local s; s="$(serial)"
  [ -n "$s" ] || die "no emulator is running -- ./tv-emulator.sh start"
  echo "$s"
}

cmd_install() {
  yes | "$SDKMANAGER" --licenses >/dev/null 2>&1 || true
  "$SDKMANAGER" --install "emulator" "$IMAGE"
}

cmd_create() {
  [ -f "$ANDROID_HOME/system-images/android-${API}/${TAG}/${ABI}/source.properties" ] \
    || die "$IMAGE is not installed -- ./tv-emulator.sh install"
  if "$AVDMANAGER" list avd -c 2>/dev/null | grep -qx "$NAME"; then
    echo "AVD $NAME already exists"
    return
  fi
  echo no | "$AVDMANAGER" create avd -n "$NAME" -k "$IMAGE" -d tv_1080p
  echo "created AVD $NAME"
}

cmd_start() {
  if [ -n "$(serial)" ]; then echo "already running as $(serial)"; return; fi
  "$EMULATOR" -avd "$NAME" -no-snapshot-save -no-boot-anim "$@" >/dev/null 2>&1 &
  echo "booting $NAME..."
  local s=""
  until [ -n "$s" ]; do s="$(serial)"; [ -n "$s" ] || sleep 2; done
  "$ADB" -s "$s" wait-for-device
  until [ "$("$ADB" -s "$s" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do
    sleep 3
  done
  echo "booted as $s"
  cmd_props
}

cmd_props() {
  local s; s="$(require_running)"
  echo "--- properties detect_device_type() reads ---"
  for p in ro.product.brand ro.product.model ro.product.device \
           ro.product.manufacturer ro.build.characteristics \
           ro.build.version.release ro.serialno; do
    printf '%-32s %s\n' "$p" "$("$ADB" -s "$s" shell getprop "$p" | tr -d '\r')"
  done
  local ch
  ch="$("$ADB" -s "$s" shell getprop ro.build.characteristics | tr -d '\r')"
  case ",$ch," in
    *,tv,*) ;;
    *) cat <<'NOTE'

NOTE: this image reports ro.build.characteristics without `tv`, so
detect_device_type() classifies it as Unknown and AppListBundle::for_device
serves the `common` list only -- the shield/ and googletv/ entries are not
offered. `emulator -prop ro.build.characteristics=tv` does NOT help: init
refuses to overwrite a ro.* property that the image already set.
NOTE
      ;;
  esac
}

# Report which catalogued packages this image actually ships, so you know what
# Optimize/Apps will have to work with before you start clicking.
cmd_catalog() {
  local s; s="$(require_running)"
  "$ADB" -s "$s" shell pm list packages 2>/dev/null | tr -d '\r' | sed 's/^package://' | sort \
    | REPO_ROOT="$REPO_ROOT" python3 -c '
import json, os, sys
pkgs = set(sys.stdin.read().split())
root = os.environ["REPO_ROOT"]
total = 0
for name in ("common", "shield", "googletv"):
    entries = json.load(open(os.path.join(root, "crates/core/data/app-lists", name + ".json")))
    hits = [a for a in entries if a["package"] in pkgs]
    total += len(hits)
    print("{}: {} of {} present".format(name, len(hits), len(entries)))
    for a in hits:
        print("    {:<45} {:<9} {}".format(a["package"], a["method"], a["risk"]))
print("catalogued packages on this image: {}".format(total))
print("uncatalogued packages (exercise Unknown safety): {}".format(len(pkgs) - total))
'
}


# Publish the guest's adbd on a host TCP port so the app can dial it the way it
# would dial a real TV. Needs `adb root`, which user builds refuse.
cmd_expose() {
  local s; s="$(require_running)"
  if ! "$ADB" -s "$s" root 2>&1 | tr -d '\r' | grep -qv "cannot run as root"; then
    cat >&2 <<NOTE
error: this image is a user build -- \`adb root\` is refused, so adbd cannot be
       told to listen on TCP (service.adb.tcp.port) and there is nothing to
       forward. Drive this emulator through its \`$s\` serial instead (the
       desktop app does exactly that), or point the mobile app at a real TV.
NOTE
    exit 1
  fi
  "$ADB" -s "$s" wait-for-device
  "$ADB" -s "$s" shell setprop service.adb.tcp.port 5555
  "$ADB" -s "$s" shell stop adbd
  "$ADB" -s "$s" shell start adbd
  "$ADB" -s "$s" wait-for-device
  "$ADB" -s "$s" forward "tcp:${HOST_PORT}" tcp:5555
  echo "guest adbd published on 127.0.0.1:${HOST_PORT}"
  echo "  host adb / desktop app  : adb connect 127.0.0.1:${HOST_PORT}"
  echo "  app in a phone emulator : 10.0.2.2:${HOST_PORT}"
}

cmd_stop() {
  local s; s="$(serial)"
  [ -n "$s" ] || { echo "nothing running"; return; }
  "$ADB" -s "$s" emu kill
  echo "stopped $s"
}

cmd_destroy() {
  cmd_stop || true
  "$AVDMANAGER" delete avd -n "$NAME"
}

case "${1:-}" in
  install) shift; cmd_install "$@" ;;
  create)  shift; cmd_create "$@" ;;
  start)   shift; cmd_start "$@" ;;
  props)   shift; cmd_props "$@" ;;
  catalog) shift; cmd_catalog "$@" ;;
  expose)  shift; cmd_expose "$@" ;;
  stop)    shift; cmd_stop "$@" ;;
  destroy) shift; cmd_destroy "$@" ;;
  *) sed -n '2,16p' "$0"; exit 1 ;;
esac
