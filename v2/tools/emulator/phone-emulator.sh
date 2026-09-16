#!/usr/bin/env bash
# Boot a phone emulator to stand in for the Pixel when testing the mobile app.
# See README.md in this directory for what it can and cannot prove -- most
# importantly, mDNS discovery does not work, so add the TV by IP:port.
#
#   ./phone-emulator.sh install   # SDK system image
#   ./phone-emulator.sh create    # create the AVD
#   ./phone-emulator.sh start     # boot it and wait for sys.boot_completed
#   ./phone-emulator.sh deploy    # build the debug APK and install it
#   ./phone-emulator.sh launch    # start the activity
#   ./phone-emulator.sh logs      # follow the Rust tracing output
#   ./phone-emulator.sh stop
#   ./phone-emulator.sh destroy
#
# Env overrides: ATV_PHONE_API (36)  ATV_PHONE_TAG (google_apis_playstore)
#                ATV_PHONE_ABI (arm64-v8a)  ATV_PHONE_DEVICE (pixel_9_pro)
#                ATV_PHONE_NAME
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/sdk}"
API="${ATV_PHONE_API:-36}"
TAG="${ATV_PHONE_TAG:-google_apis_playstore}"
ABI="${ATV_PHONE_ABI:-arm64-v8a}"
DEVICE="${ATV_PHONE_DEVICE:-pixel_9_pro}"
NAME="${ATV_PHONE_NAME:-atvopt-phone-${API}}"
IMAGE="system-images;android-${API};${TAG};${ABI}"
PKG="com.atvoptimizer.mobile"
MOBILE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../mobile" && pwd)"

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
AVDMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager"
EMULATOR="$ANDROID_HOME/emulator/emulator"
ADB="$ANDROID_HOME/platform-tools/adb"
[ -x "$ADB" ] || ADB="$(command -v adb)"

die() { echo "error: $*" >&2; exit 1; }

# Phone AVDs get a fixed console port so a TV emulator can run alongside on the
# default 5554 without the two fighting over serials.
PORT="${ATV_PHONE_CONSOLE_PORT:-5584}"
SERIAL="emulator-${PORT}"

running() { "$ADB" devices | grep -q "^${SERIAL}"; }

require_running() { running || die "phone emulator is not running -- ./phone-emulator.sh start"; }

cmd_install() {
  # sdkmanager needs a writable temp dir; a restricted TMPDIR makes it fail with
  # a NullPointerException out of FileOpUtils.getNewTempDir.
  local tmp; tmp="$(mktemp -d)"
  yes | TMPDIR="$tmp" "$SDKMANAGER" --licenses >/dev/null 2>&1 || true
  TMPDIR="$tmp" "$SDKMANAGER" --install "emulator" "$IMAGE"
  rm -rf "$tmp"
}

cmd_create() {
  [ -f "$ANDROID_HOME/system-images/android-${API}/${TAG}/${ABI}/source.properties" ] \
    || die "$IMAGE is not installed -- ./phone-emulator.sh install"
  if "$AVDMANAGER" list avd -c 2>/dev/null | grep -qx "$NAME"; then
    echo "AVD $NAME already exists"
    return
  fi
  echo no | "$AVDMANAGER" create avd -n "$NAME" -k "$IMAGE" -d "$DEVICE"
  echo "created AVD $NAME"
}

cmd_start() {
  if running; then echo "already running as $SERIAL"; return; fi
  "$EMULATOR" -avd "$NAME" -port "$PORT" -no-snapshot-save -no-boot-anim "$@" >/dev/null 2>&1 &
  echo "booting $NAME on $SERIAL..."
  until running; do sleep 2; done
  "$ADB" -s "$SERIAL" wait-for-device
  until [ "$("$ADB" -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do
    sleep 3
  done
  echo "booted as $SERIAL"
  cat <<NOTE

Reachability from inside the guest (both verified on this setup):
  10.0.2.2        the Mac's loopback
  192.168.x.y     any LAN host, so a real TV's wireless-ADB endpoint works

mDNS does NOT cross the emulator's NAT, so the discovery scan finds nothing.
Add the TV by IP:port from the TV's Wireless debugging screen.
NOTE
}

cmd_deploy() {
  require_running
  local ndk apk
  ndk="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort | tail -1)"
  [ -n "$ndk" ] || die "no NDK under $ANDROID_HOME/ndk"
  ( cd "$MOBILE_DIR" && PATH="$ANDROID_HOME/platform-tools:$PATH" NDK_HOME="$ndk" \
      npx tauri android build --apk --debug --target aarch64 )
  apk="$(ls -t "$MOBILE_DIR"/src-tauri/gen/android/app/build/outputs/apk/*/debug/*.apk 2>/dev/null | head -1)"
  [ -n "$apk" ] || die "no debug APK produced"
  echo "installing $apk"
  "$ADB" -s "$SERIAL" install -r -d "$apk"
  cmd_launch
}

cmd_launch() {
  require_running
  "$ADB" -s "$SERIAL" shell am start -n "$PKG/.MainActivity"
}

cmd_logs() {
  require_running
  "$ADB" -s "$SERIAL" logcat -v time 'RustStdoutStderr:V' '*:S'
}

cmd_stop() {
  running || { echo "nothing running"; return; }
  "$ADB" -s "$SERIAL" emu kill
  echo "stopped $SERIAL"
}

cmd_destroy() {
  cmd_stop || true
  "$AVDMANAGER" delete avd -n "$NAME"
}

case "${1:-}" in
  install) shift; cmd_install "$@" ;;
  create)  shift; cmd_create "$@" ;;
  start)   shift; cmd_start "$@" ;;
  deploy)  shift; cmd_deploy "$@" ;;
  launch)  shift; cmd_launch "$@" ;;
  logs)    shift; cmd_logs "$@" ;;
  stop)    shift; cmd_stop "$@" ;;
  destroy) shift; cmd_destroy "$@" ;;
  *) sed -n '2,17p' "$0"; exit 1 ;;
esac
