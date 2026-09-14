#!/usr/bin/env bash
# Open an agent thread AND bring its editor window to the front, on KDE.
#
# Point the tray at this script:
#
#     ZEO_SYSTRAY_OPEN_CMD='/usr/share/zeo-systray/zeo-systray-open-kde.sh {session} {cwd}'
#
# Why a script instead of doing it in the daemon: raising a window is not
# something an application can decide on Wayland. The compositor rejects an
# activation request from a process that does not already have focus -- that is
# the whole point of focus-stealing prevention -- so the editor asking to be
# raised gets, at best, a blinking task-bar entry. What *can* do it is the
# compositor itself. This runs a throwaway KWin script, which executes inside
# KWin and is therefore not a foreign process asking for focus.
#
# That makes this file KDE-specific by nature, which is also why it is not
# compiled into the daemon: a tray that works on six desktops should not carry
# one desktop's window manager inside it.
#
# Exit is always 0: an opener that fails must never look like an agent failure.

set -uo pipefail

session="${1:-}"
cwd="${2:-}"

[[ -n "${session}" ]] || exit 0

# The window caption is "<project> — <open file>", and the file half changes
# whenever a tab does. Match on the project half only: it is the stable part,
# and it is exactly the basename of the working directory the hook reported.
project="$(basename -- "${cwd:-}")"

raise_window() {
	command -v busctl >/dev/null 2>&1 || return 0
	[[ -n "${project}" ]] || return 0

	local tmp name
	tmp="$(mktemp --suffix=.js)" || return 0
	name="zeoSystrayRaise$$"

	# Embedded as JSON so a project name with quotes cannot break out of the
	# script it is pasted into.
	local json_project
	json_project="$(printf '%s' "${project}" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null)" || {
		rm -f "${tmp}"
		return 0
	}

	cat >"${tmp}" <<-EOF
		var want = ${json_project};
		var wins = workspace.windowList();
		for (var i = 0; i < wins.length; i++) {
		    var cls = String(wins[i].resourceClass || "").toLowerCase();
		    if (cls.indexOf("zed") === -1 && cls.indexOf("zeo") === -1) continue;
		    var cap = String(wins[i].caption);
		    if (cap.split("—")[0].trim() !== want) continue;
		    // Switching desktop first is the part that matters: activating a
		    // window the screen is not showing leaves the user looking at the
		    // desktop they were already on.
		    if (wins[i].desktops && wins[i].desktops.length > 0) {
		        workspace.currentDesktop = wins[i].desktops[0];
		    }
		    workspace.activeWindow = wins[i];
		    break;
		}
	EOF

	busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting \
		loadScript ss "${tmp}" "${name}" >/dev/null 2>&1
	busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting \
		start >/dev/null 2>&1
	busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting \
		unloadScript s "${name}" >/dev/null 2>&1
	rm -f "${tmp}"
}

# Raise first, then hand over the link. The other order works too, but this way
# the window is already in front when the thread switches, so the switch is
# something the user sees happen rather than something they find afterwards.
raise_window
xdg-open "zed://agent?session=${session}" >/dev/null 2>&1

exit 0
