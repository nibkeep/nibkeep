"""An LLDB command for scripts/test-on-device.py: `wait_for_exit` keeps LLDB attached to a
test app on an iPhone until the app ends.

iOS ends an app that has not finished launching after 20 seconds of wall clock time, and a
test executable, which is the whole app, never does; a debugger attached from the start
turns that watchdog off. Loaded with `command script import`, it lets the app go on after
the stops that signals make, and ends it if it stops with an exception, a crash, which it
reports.
"""

import lldb


def wait_for_exit(debugger, command, result, internal_dict):
	"""Waits until the selected target's process exits, or ends it after a crash."""
	process = debugger.GetSelectedTarget().GetProcess()
	listener = debugger.GetListener()
	event = lldb.SBEvent()
	while True:
		state = process.GetState()
		if state in (lldb.eStateExited, lldb.eStateDetached, lldb.eStateInvalid):
			return
		if state in (lldb.eStateStopped, lldb.eStateCrashed):
			thread = process.GetSelectedThread()
			reason = thread.GetStopReason() if thread else lldb.eStopReasonNone
			if state == lldb.eStateCrashed or reason == lldb.eStopReasonException:
				result.AppendMessage(f"the test app stopped: {thread.GetStopDescription(200) if thread else state}")
				process.Kill()
				return
			process.Continue()
		listener.WaitForEvent(1, event)


def __lldb_init_module(debugger, internal_dict):
	debugger.HandleCommand(f"command script add -f {__name__}.wait_for_exit wait_for_exit")
