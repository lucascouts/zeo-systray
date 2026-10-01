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
# whenever a tab does. Match on the project half only: it is the stable part.
# The hook reports the directory the session runs in, which is the project
# root or somewhere below it -- a session started in app-emulation/d7vk of a
# project named bentoo -- so the candidates are that directory's name and then
# each parent's, nearest first, stopping short of $HOME. The first candidate a
# window matches wins, which keeps a nested project ahead of its parent.
candidates=()
dir="${cwd%/}"
while [[ -n "${dir}" && "${dir}" != "/" && "${dir}" != "${HOME%/}" ]]; do
	candidates+=("$(basename -- "${dir}")")
	dir="$(dirname -- "${dir}")"
done

raise_window() {
	command -v busctl >/dev/null 2>&1 || return 0
	((${#candidates[@]} > 0)) || return 0

	local tmp name
	tmp="$(mktemp --suffix=.js)" || return 0
	name="zeoSystrayRaise$$"

	# Embedded as JSON so a directory name with quotes cannot break out of the
	# script it is pasted into.
	local json_candidates
	json_candidates="$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "${candidates[@]}" 2>/dev/null)" || {
		rm -f "${tmp}"
		return 0
	}

	cat >"${tmp}" <<-EOF
		var wanted = ${json_candidates};
		var wins = workspace.windowList();
		var hit = null;
		for (var c = 0; c < wanted.length && !hit; c++) {
		    for (var i = 0; i < wins.length; i++) {
		        var cls = String(wins[i].resourceClass || "").toLowerCase();
		        if (cls.indexOf("zed") === -1 && cls.indexOf("zeo") === -1) continue;
		        if (String(wins[i].caption).split("—")[0].trim() === wanted[c]) {
		            hit = wins[i];
		            break;
		        }
		    }
		}
		if (hit) {
		    // Switching desktop first is the part that matters: activating a
		    // window the screen is not showing leaves the user looking at the
		    // desktop they were already on.
		    if (hit.desktops && hit.desktops.length > 0) {
		        workspace.currentDesktop = hit.desktops[0];
		    }
		    if (hit.minimized) {
		        hit.minimized = false;
		    }
		    workspace.activeWindow = hit;
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

# The directory rides in a URL query, so it is percent-encoded: a bare `&`
# would end the parameter and a `+` would decode as a space.
link="zed://agent?session=${session}"
if [[ -n "${cwd}" ]]; then
	cwd_query="$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe="/"))' "${cwd}" 2>/dev/null)" &&
		link="${link}&cwd=${cwd_query}"
fi

# The editor's own CLI when one is installed, rather than xdg-open, and always
# with zed://. Zeo registers zeo:// with the desktop, so xdg-open zed:// finds
# nobody once Zeo replaces Zed -- and Zeo's own handler matches agent links
# only in their zed:// spelling, so zeo://agent would open nothing either.
# Both CLIs accept zed://, which is what makes one link work in both editors.
# Package CLIs, in order: zeo (app-editors/zeo, zeo-bin), zedit (zed),
# zedit-bin (zed-bin); only one of those packages installs at a time.
# ZEO_SYSTRAY_KDE_LINK_OPENER names a different program outright.
opener="${ZEO_SYSTRAY_KDE_LINK_OPENER:-}"
if [[ -z "${opener}" ]]; then
	opener="xdg-open"
	for cli in zeo zedit zedit-bin; do
		if command -v "${cli}" >/dev/null 2>&1; then
			opener="${cli}"
			break
		fi
	done
fi
"${opener}" "${link}" >/dev/null 2>&1

# Once more after the link: when no window held the project, Zed opens one for
# it, and that window did not exist for the first attempt to find. Raising a
# window that is already in front changes nothing, so the repeat is harmless.
sleep 1
raise_window

exit 0
