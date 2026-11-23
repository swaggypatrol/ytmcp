#!/usr/bin/env bash
# G6 installed-package smoke test (run AFTER the package is installed; installs nothing).
#   YTMCP_QA_EVIDENCE_SUBDIR=0.1.0-3 tools/qa/g6-installed.sh [--run final|dryrun]
# Launches through KDE (kioclient exec on the desktop entry), checks it runs from /opt
# without the dev tree, plays (volume 0, stream moved to a null sink), single instance
# (second launch activates the first or KWin signals attention), quit leaves no
# orphans, relaunch plays again.
# Writes artifacts/m0/[dryrun/]g6.json. Needs: kioclient, qdbus6, busctl, pactl, jq.
set -uo pipefail

RUN=final
[[ "${1:-}" == "--run" ]] && RUN="${2:-final}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
OUT_DIR="$REPO/artifacts/m0"
if [[ -n "${YTMCP_QA_EVIDENCE_SUBDIR:-}" ]]; then
  if [[ ! "$YTMCP_QA_EVIDENCE_SUBDIR" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]]; then
    echo 'YTMCP_QA_EVIDENCE_SUBDIR must name a subdirectory of artifacts/m0' >&2
    exit 2
  fi
  OUT_DIR="$OUT_DIR/$YTMCP_QA_EVIDENCE_SUBDIR"
elif [[ "$RUN" != "final" ]]; then
  OUT_DIR="$OUT_DIR/$RUN"
fi
mkdir -p "$OUT_DIR"
DESKTOP=/usr/share/applications/local.ytmcp.Desktop.desktop
BIN=/opt/ytmcp/ytmcp-player
DAEMON=/opt/ytmcp/resources/bin/ytm-daemon
MPRIS=org.mpris.MediaPlayer2.ytmcp
MP_PATH=/org/mpris/MediaPlayer2
SINK=ytmcp_null
RESTORE_TOLERANCE_US=2000000
RESTORE_SETTLE_SECONDS=18
CHECKS=()
NOTES=()

check() { # id status what measured-json
  CHECKS+=("$(jq -nc --arg id "$1" --arg st "$2" --arg what "$3" --argjson m "${4:-null}" '{id:$id,status:$st,what:$what,measured:$m}')")
}
pass_if() { [[ "$1" == "true" ]] && echo PASS_TARGET || echo FAIL; }

main_pids() { # browser processes of the installed app (no --type=)
  local p cmdline
  for p in $(pgrep -f "^$BIN" 2>/dev/null); do
    cmdline="$(tr '\0' ' ' 2>/dev/null <"/proc/$p/cmdline")" || continue
    [[ -n "$cmdline" && "$cmdline" != *'--type='* ]] && echo "$p"
  done
}
main_pid() { local pids; pids="$(main_pids)"; [[ -n "$pids" ]] && printf '%s\n' "${pids%%$'\n'*}"; }
app_pids() { { pgrep -f "^$BIN"; pgrep -f "^$DAEMON"; } 2>/dev/null | sort -u; }
wait_for() { # seconds cmd...
  local n=$(( $1 * 4 )); shift
  for ((i = 0; i < n; i++)); do "$@" >/dev/null 2>&1 && return 0; sleep 0.25; done
  return 1
}
mpris_up() { busctl --user list --no-legend | awk '{print $1}' | grep -qx "$MPRIS"; }
mpris_get() { busctl --user -j get-property "$MPRIS" "$MP_PATH" org.mpris.MediaPlayer2.Player "$1" 2>/dev/null | jq -c '.data'; }
mpris_call() { busctl --user call "$MPRIS" "$MP_PATH" org.mpris.MediaPlayer2.Player "$1" >/dev/null 2>&1; }

mpris_sample() {
  local state position metadata fingerprint=null
  local -a hashes
  state="$(mpris_get PlaybackStatus)"
  position="$(mpris_get Position)"
  # MPRIS track IDs are per playback instance. Keep fingerprints of the stable fields
  # in memory so a mismatch can be diagnosed without putting their values in evidence.
  if metadata="$(mpris_get Metadata | jq -ce 'select(."xesam:title".data | type == "string") | [."xesam:title".data, ."xesam:artist".data, ."xesam:album".data, ."mpris:length".data]')"; then
    mapfile -t hashes < <(jq -c '.[]' <<<"$metadata" | while IFS= read -r value; do
      printf '%s' "$value" | sha256sum | cut -d' ' -f1
    done)
    fingerprint="$(jq -nc --args '{title:$ARGS.positional[0],artist:$ARGS.positional[1],album:$ARGS.positional[2],duration:$ARGS.positional[3]}' "${hashes[@]}")"
  fi
  jq -nc --argjson state "${state:-null}" --argjson pos "${position:-null}" --argjson fingerprint "$fingerprint" \
    '{status:$state,positionUs:$pos,fingerprint:$fingerprint}'
}

restore_log_counts() {
  local pid="$1" since="$2" counts
  if [[ -n "$pid" ]] && counts="$(journalctl --user "_PID=$pid" --since="$since" --no-pager -o cat 2>/dev/null |
    jq -Rn 'reduce (inputs | fromjson? | select(.event == "daemon_stderr") | .detail.line // "") as $line
      ({daemonLines:0,restoreStarts:0,seekTimeouts:0,restoreFailures:0};
       .daemonLines += 1
       | .restoreStarts += (if $line | contains("restoring last played track paused") then 1 else 0 end)
       | .seekTimeouts += (if $line | contains("command Seek timed out waiting for player observation") then 1 else 0 end)
       | .restoreFailures += (if $line | contains("paused restore did not settle") then 1 else 0 end))
       | select(.daemonLines > 0)')"; then
    printf '%s\n' "${counts:-null}"
  else
    echo null
  fi
}

compare_paused_restore() {
  jq -nc --argjson saved "$1" --argjson actual "$2" --argjson autoplay "$3" --argjson limit "$RESTORE_TOLERANCE_US" '
    (if $saved.fingerprint and $actual.fingerprint then $saved.fingerprint == $actual.fingerprint else null end) as $same
    | (if $same == null then null else $saved.fingerprint | with_entries(
        .value = (.value == $actual.fingerprint[.key])) end) as $field_matches
    | (if ($saved.positionUs | type) == "number" and ($actual.positionUs | type) == "number"
       then ($actual.positionUs - $saved.positionUs | fabs) else null end) as $delta
    | ($autoplay != true and ($saved.positionUs|type) == "number" and $saved.positionUs > $limit) as $expected
    | {sameTrackMetadata:$same,metadataFieldMatches:$field_matches,positionDeltaUs:$delta,toleranceUs:$limit,
       autoplayOnLaunch:$autoplay,expected:$expected,
       result:(if $expected | not then "NOT_RUN"
               elif $saved.status == "Paused" and $actual.status == "Paused" and $same == true
                    and $delta != null and $delta <= $limit then "PASS_TARGET"
               else "FAIL" end)}'
}

read_launch_autoplay() {
  local config_root="${XDG_CONFIG_HOME:-$HOME/.config}" autoplay
  [[ -z "${YTMCP_DATA_DIR:-}" ]] || config_root="$YTMCP_DATA_DIR/config"
  # The daemon defaults to paused restore when no persisted settings are available.
  if autoplay="$(jq -sr 'length == 1 and .[0].autoplayOnLaunch == true' "$config_root/ytmcp/settings.json" 2>/dev/null)"; then
    printf '%s\n' "$autoplay"
  else
    echo false
  fi
}

kwin_script() { # body -> JSON lines printed by out()
  local marker="YTMQAG6$RANDOM$RANDOM" f since id
  f="$(mktemp --suffix=.js)"
  printf 'function out(o){print("%s "+JSON.stringify(o));}\ntry{%s}catch(e){out({error:String(e)});}\nout({done:true});\n' "$marker" "$1" >"$f"
  since="@$(( $(date +%s) - 2 ))"
  id="$(qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$f" "ytmqa_$marker")"
  qdbus6 org.kde.KWin "/Scripting/Script$id" org.kde.kwin.Script.run >/dev/null 2>&1
  sleep 1
  journalctl --user --since="$since" -o cat --no-pager | grep "$marker" | sed "s/^.*$marker //" | grep -v '"done":true'
  qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript "ytmqa_$marker" >/dev/null 2>&1
  rm -f "$f"
}
kwin_app_windows() { kwin_script "for (const w of workspace.windowList()) { if (w.pid === $1 && w.normalWindow) out({id:String(w.internalId),cls:String(w).replace(/\\(0x[0-9a-f]+\\)/,''),desktopFileName:w.desktopFileName,resourceClass:w.resourceClass,minimized:w.minimized,active:w.active,demandsAttention:w.demandsAttention}); }"; }
kwin_app_window() { kwin_app_windows "$1" | head -1; }
kwin_minimize() { kwin_script "for (const w of workspace.windowList()) { if (w.pid === $1 && w.normalWindow) { w.minimized = true; out({min:true}); } }" >/dev/null; }

dev_tree_refs() { # count files from the dev tree / tmp worktrees mapped or open by app processes
  local n=0 p
  for p in $(app_pids); do
    n=$(( n + $( { cat "/proc/$p/maps" 2>/dev/null; ls -l "/proc/$p/fd" 2>/dev/null; readlink "/proc/$p/cwd"; } | grep -c -e "$REPO" -e "/tmp/claude-" -e "Youtube Music Player" ) ))
  done
  echo "$n"
}

move_streams_to_sink() { # move every sink input of the app to the null sink
  local pids ids
  pids="$(app_pids | paste -sd'|')"
  [[ -z "$pids" ]] && return
  ids="$(pactl -f json list sink-inputs | jq -r --arg re "^($pids)\$" '.[] | select((.properties["application.process.id"] // "") | test($re)) | .index')"
  for i in $ids; do pactl move-sink-input "$i" "$SINK" >/dev/null 2>&1; done
}
stream_on_null() {
  local pids
  pids="$(app_pids | paste -sd'|')"
  pactl -f json list sink-inputs | jq -e --arg re "^($pids)\$" --arg sinkidx "$(pactl -f json list short sinks | jq -r --arg s "$SINK" '.[] | select(.name==$s) | .index')" \
    '[.[] | select((.properties["application.process.id"] // "") | test($re)) | select((.sink|tostring) == $sinkidx and .corked == false)] | length > 0' >/dev/null
}

play_check() { # label -> sets PLAY_JSON
  local label="$1" pid="$2" since="$3" saved="${4:-null}"
  local vol0 st pos0 pos1 before logs restore=null first_call=false retry=false ok=false stream=false
  local restore_deadline autoplay
  if ! has_null_sink_target "$pid"; then
    PLAY_JSON="$(jq -nc --arg label "$1" '{label:$label,ok:false,reason:"KDE launch did not inherit the QA null sink"}')"
    return
  fi
  vol0="$(mpris_get Volume)"
  busctl --user set-property "$MPRIS" "$MP_PATH" org.mpris.MediaPlayer2.Player Volume d 0 >/dev/null 2>&1
  # Play is idempotent: wait until the page has a track (launch restore), retry once if refused early.
  wait_for 30 bash -c "busctl --user -j get-property $MPRIS $MP_PATH org.mpris.MediaPlayer2.Player Metadata | jq -e '.data[\"mpris:trackid\"].data | test(\"NoTrack\") | not'"
  before="$(mpris_sample)"
  if [[ "$saved" != null ]]; then
    # The muted play/seek/pause transaction and the page's metadata can settle separately.
    # Capture the same-track paused position before sending an explicit playback command.
    autoplay="$(read_launch_autoplay)"
    restore_deadline=$((SECONDS + RESTORE_SETTLE_SECONDS))
    restore="$(compare_paused_restore "$saved" "$before" "$autoplay")"
    while [[ "$(jq -r .result <<<"$restore")" == FAIL ]] && ((SECONDS < restore_deadline)); do
      sleep 0.25
      before="$(mpris_sample)"
      restore="$(compare_paused_restore "$saved" "$before" "$autoplay")"
    done
  fi
  if mpris_call Play; then first_call=true; fi
  if ! wait_for 8 bash -c "busctl --user -j get-property $MPRIS $MP_PATH org.mpris.MediaPlayer2.Player PlaybackStatus | jq -e '.data==\"Playing\"'"; then
    retry=true
    mpris_call Play
  fi
  for ((i = 0; i < 80; i++)); do move_streams_to_sink; stream_on_null && { stream=true; break; }; sleep 0.25; done
  wait_for 20 bash -c "busctl --user -j get-property $MPRIS $MP_PATH org.mpris.MediaPlayer2.Player PlaybackStatus | jq -e '.data==\"Playing\"'"
  st="$(mpris_get PlaybackStatus)"; pos0="$(mpris_get Position)"; sleep 3; pos1="$(mpris_get Position)"
  mpris_call Pause
  sleep 1
  [[ -n "$vol0" && "$vol0" != "null" ]] && busctl --user set-property "$MPRIS" "$MP_PATH" org.mpris.MediaPlayer2.Player Volume d "$vol0" >/dev/null 2>&1
  logs="$(restore_log_counts "$pid" "$since")"
  [[ "$st" == '"Playing"' && "$stream" == true && "${pos1:-0}" -gt "${pos0:-0}" ]] && ok=true
  PLAY_JSON="$(jq -nc --arg l "$label" --argjson st "${st:-null}" --argjson p0 "${pos0:-null}" --argjson p1 "${pos1:-null}" \
    --argjson s "$stream" --argjson ok "$ok" --argjson v "${vol0:-null}" --argjson before "$before" \
    --argjson logs "$logs" --argjson restore "$restore" --argjson first "$first_call" --argjson retry "$retry" '
    {label:$l,status:$st,positionUs:[$p0,$p1],streamOnNullSink:$s,volumeRestoredTo:$v,
     beforePlay:($before | del(.fingerprint)),firstPlayCallSucceeded:$first,playRetried:$retry,
     restoreJournal:$logs,pausedRestore:$restore,
     ok:($ok and ($logs.seekTimeouts // 0) == 0 and $restore.result != "FAIL")}')"
}

# KIO passes its launch environment to the desktop entry's process. Set routing before
# Electron starts so a persisted autoplay preference also uses the temporary sink.
launch_kde() { PULSE_SINK="$SINK" kioclient exec "$DESKTOP" >/dev/null 2>&1 & }
has_null_sink_target() {
  local pid
  [[ -n "$1" ]] || return 1
  # Chromium can reuse its original environment storage when setting its process
  # title. The running daemon is its direct child, spawned with process.env.
  for pid in $(pgrep -P "$1" -f "^$DAEMON"); do
    if tr '\0' '\n' <"/proc/$pid/environ" | grep -Fx "PULSE_SINK=$SINK" >/dev/null; then
      return 0
    fi
  done
  return 1
}

quit_and_check() { # label -> sets QUIT_JSON
  local t0 left
  PRE_QUIT="$(mpris_sample)"
  t0=$(date +%s%N)
  busctl --user call "$MPRIS" "$MP_PATH" org.mpris.MediaPlayer2 Quit >/dev/null 2>&1
  wait_for 25 bash -c "[[ -z \"\$({ pgrep -f '^$BIN'; pgrep -f '^$DAEMON'; } 2>/dev/null)\" ]]"
  sleep 2
  left="$(app_pids | paste -sd, )"
  QUIT_JSON="$(jq -nc --arg l "$1" --arg left "$left" --argjson ms "$(( ($(date +%s%N) - t0) / 1000000 ))" --argjson before "$PRE_QUIT" \
    '{label:$l,leftoverPids:($left|split(",")|map(select(.!=""))),quitMs:$ms,beforeQuit:($before | del(.fingerprint))}')"
}

cleanup() {
  [[ -n "${SINK_MOD:-}" ]] && pactl unload-module "$SINK_MOD" >/dev/null 2>&1
}
trap cleanup EXIT

# ---------------------------------------------------------------- preconditions
PKG="$(pacman -Q ytmcp 2>/dev/null || echo MISSING)"
if [[ "$PKG" == MISSING || ! -x "$BIN" || ! -f "$DESKTOP" ]]; then
  check G6.installed BLOCKED "ytmcp package installed (pacman -Q), $BIN and $DESKTOP present" "$(jq -nc --arg p "$PKG" '{package:$p}')"
  jq -n --arg run "$RUN" --argjson checks "[$(IFS=,; echo "${CHECKS[*]}")]" '{gate:"G6",run:($run|ascii_upcase),result:"BLOCKED",checks:$checks}' >"$OUT_DIR/g6.json"
  echo "BLOCKED: package not installed"; exit 2
fi
if [[ -n "$(app_pids)" ]]; then echo "an installed instance is already running; quit it first" >&2; exit 3; fi
check G6.installed PASS_TARGET "ytmcp package installed" "$(jq -nc --arg p "$PKG" --arg sha "$(sha256sum "$BIN" | cut -c1-16)" '{package:$p,binSha256Prefix:$sha}')"
if ! pactl list short sinks | awk '{print $2}' | grep -qx "$SINK"; then
  if ! SINK_MOD="$(pactl load-module module-null-sink sink_name=$SINK sink_properties=device.description=YTMGlassNull)"; then
    echo "BLOCKED: temporary null sink unavailable" >&2
    exit 2
  fi
fi

# ---------------------------------------------------------------- first launch through KDE
FIRST_SINCE="$(date '+%Y-%m-%d %H:%M:%S.%6N')"
launch_kde
wait_for 30 main_pid; MAIN="$(main_pid)"
wait_for 60 mpris_up
EXE="$(readlink "/proc/$MAIN/exe" 2>/dev/null)"
DEXE="$(for p in $(pgrep -f "^$DAEMON"); do readlink "/proc/$p/exe"; done | head -1)"
UNIT="$(cut -d: -f3 "/proc/$MAIN/cgroup" 2>/dev/null | awk -F/ '{print $NF}')"
REFS="$(dev_tree_refs)"
WIN=""; for ((i = 0; i < 30; i++)); do WIN="$(kwin_app_window "$MAIN")"; [[ -n "$WIN" && "$WIN" != null ]] && break; sleep 0.5; done
check G6.kdeLaunch "$(pass_if "$([[ -n "$MAIN" ]] && mpris_up && has_null_sink_target "$MAIN" && echo true)")" "KDE launcher starts the app with QA audio routing and its MPRIS service" "$(jq -nc --arg u "$UNIT" --argjson up "$(mpris_up && echo true || echo false)" --argjson routed "$(has_null_sink_target "$MAIN" && echo true || echo false)" '{scope:$u,mprisUp:$up,nullSinkTargetInherited:$routed}')"
check G6.fromOpt "$(pass_if "$([[ "$EXE" == /opt/ytmcp/* && "$DEXE" == "$DAEMON" && "$REFS" == 0 ]] && echo true)")" "runtime, sidecar and resources come from /opt; no dev-tree file is mapped or open" "$(jq -nc --arg e "$EXE" --arg d "$DEXE" --argjson r "$REFS" '{electronExe:$e,daemonExe:$d,devTreeRefs:$r}')"
check G6.identity "$(pass_if "$(echo "$WIN" | jq -r '(.desktopFileName=="local.ytmcp.Desktop") and (.cls|startswith("KWin::XdgToplevelWindow"))' 2>/dev/null)")" "window is native Wayland with desktopFileName local.ytmcp.Desktop" "${WIN:-null}"
play_check first "$MAIN" "$FIRST_SINCE"; check G6.play "$(pass_if "$(echo "$PLAY_JSON" | jq -r .ok)")" "installed app plays (MPRIS Play -> Playing, position advances, stream active)" "$PLAY_JSON"

# ---------------------------------------------------------------- single instance
kwin_minimize "$MAIN"; sleep 1
WIN_BEFORE="$(kwin_app_windows "$MAIN" | jq -cs '.')"
ACTIVATION_SINCE="$(date '+%Y-%m-%d %H:%M:%S.%6N')"
launch_kde; sleep 6
MAINS_AFTER="$(main_pids | jq -Rcs 'split("\n") | map(select(length > 0))')"
WIN_AFTER="$(kwin_app_windows "$MAIN" | jq -cs '.')"
ACTIVATIONS="$(journalctl --user _PID="$MAIN" --since="$ACTIVATION_SINCE" --no-pager -o cat --grep='"event":"single_instance_activation_requested"' | jq -s 'length')"
SINGLE_INSTANCE="$(jq -nc --arg main "$MAIN" --argjson mains "$MAINS_AFTER" --argjson before "$WIN_BEFORE" --argjson after "$WIN_AFTER" --argjson events "$ACTIVATIONS" '
  ($before[0]) as $prior | ($after[0]) as $current |
  {mainBefore:$main, mainProcessesAfter:$mains, windowsBefore:$before, windowsAfter:$after, activationRequests:$events,
    ok: ($mains == [$main] and $events == 1 and ($before|length) == 1 and ($after|length) == 1
      and $prior.minimized == true and ($prior.id|length) > 0 and $prior.id == $current.id
      and ($current.minimized == false or
        (($current.cls | startswith("KWin::XdgToplevelWindow")) and $prior.demandsAttention == false and $current.demandsAttention == true)))}
')"
check G6.singleInstance "$(pass_if "$(echo "$SINGLE_INSTANCE" | jq -r .ok)")" "second KDE launch reaches the sole existing instance and restores its window or requests attention in KWin" "$SINGLE_INSTANCE"

# ---------------------------------------------------------------- quit, relaunch, play, quit
quit_and_check first; Q1="$QUIT_JSON"
check G6.quitNoOrphans "$(pass_if "$(echo "$Q1" | jq -r '.leftoverPids|length==0')")" "MPRIS Quit leaves no app/sidecar/crashpad process" "$Q1"
RELAUNCH_SINCE="$(date '+%Y-%m-%d %H:%M:%S.%6N')"
launch_kde
wait_for 30 main_pid; wait_for 60 mpris_up
play_check relaunch "$(main_pid)" "$RELAUNCH_SINCE" "$PRE_QUIT"; check G6.relaunchPlay "$(pass_if "$(echo "$PLAY_JSON" | jq -r .ok)")" "after relaunch the installed app still plays; a measured paused restore matches the saved track and position" "$PLAY_JSON"
quit_and_check second; Q2="$QUIT_JSON"
check G6.quitNoOrphans2 "$(pass_if "$(echo "$Q2" | jq -r '.leftoverPids|length==0')")" "second quit leaves no orphan process" "$Q2"

# ---------------------------------------------------------------- verdict
ALL="[$(IFS=,; echo "${CHECKS[*]}")]"
RESULT="$(echo "$ALL" | jq -r 'if any(.[]; .status=="FAIL") then "FAIL" elif any(.[]; .status=="BLOCKED") then "BLOCKED" elif any(.[]; .status=="NOT_RUN") then "NOT_RUN" else "PASS_TARGET" end')"
jq -n --arg run "$RUN" --arg r "$RESULT" --arg at "$(date -Iseconds)" --arg pkg "$PKG" --argjson checks "$ALL" \
  '{gate:"G6",name:"Installed package smoke test",run:($run|ascii_upcase),countsForM0:($run=="final"),result:$r,generatedAt:$at,package:$pkg,checks:$checks}' >"$OUT_DIR/g6.json"
echo "G6 $RESULT -> $OUT_DIR/g6.json"
[[ "$RESULT" == "PASS_TARGET" ]]
