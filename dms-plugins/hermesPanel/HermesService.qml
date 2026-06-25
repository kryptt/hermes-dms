pragma Singleton

import QtQuick
import Quickshell
import Quickshell.Io

// Long-lived bridge to the hermes-dms daemon. Spawns `hermes-dms-ctl stream`
// (a persistent Unix-socket relay), writes JSON-lines commands to its stdin,
// and parses JSON-lines events from its stdout.
//
// This singleton is the SOURCE OF TRUTH for conversation state (messages,
// streaming index, session id, model). The panel view is recreated every time
// the popout opens/closes, so anything stored in the view is lost — keeping the
// chat history here is what makes it survive open/close.
Singleton {
    id: root

    // --- Observable state ---
    property bool connected: false     // Hermes reachable (from status events)
    property bool daemonReady: false   // the ctl stream process is alive
    property bool busy: false
    property string statusText: "Connecting…"
    property string sessionId: ""
    property bool popoutVisible: false
    property var models: []            // ollama-router catalog: [{id, loaded, active}]
    property string selectedModel: ""  // "" = Hermes default (user's explicit pick)
    property string currentModel: ""   // model actually bound to the current session
    property var sessions: []          // desktop sessions for the switcher

    // Persistent chat transcript. Entries: { msgRole, msgContent }.
    property ListModel messages: ListModel {}
    // Index of the assistant message currently streaming (-1 = none).
    property int streamIndex: -1

    // --- Internal ---
    property string _pendingMessage: ""
    property string _currentRequestId: ""
    // When true, the next `sessions` reply adopts the most-recent desktop
    // session (used once on connect so history/model survive a daemon restart).
    property bool _adoptOnList: false

    // What the model pill shows: explicit pick, else the session's real model.
    function activeModelLabel() {
        return selectedModel || currentModel || "model";
    }

    function _uuid() {
        return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, function (c) {
            var r = Math.random() * 16 | 0;
            var v = c === "x" ? r : (r & 0x3 | 0x8);
            return v.toString(16);
        });
    }

    function _send(obj) {
        if (!ctlProc.running)
            return false;
        ctlProc.write(JSON.stringify(obj) + "\n");
        return true;
    }

    function sendMessage(text) {
        if (busy || !text.trim())
            return;
        messages.append({ msgRole: "user", msgContent: text });
        busy = true;
        statusText = "Thinking…";
        if (sessionId) {
            _startChat(text);
        } else {
            // No session yet: create one, then flush this message. Track the
            // create's request_id so a failed creation surfaces as an error and
            // clears `busy` instead of hanging. No title: the daemon assigns a
            // unique "[Desktop] <id>" (a fixed title collides — Hermes requires
            // unique titles).
            _pendingMessage = text;
            _currentRequestId = _uuid();
            _send({ type: "session_create", request_id: _currentRequestId });
        }
    }

    function _startChat(text) {
        _currentRequestId = _uuid();
        messages.append({ msgRole: "assistant", msgContent: "" });
        streamIndex = messages.count - 1;
        _send({ type: "chat", request_id: _currentRequestId, session_id: sessionId, message: text });
    }

    function cancel() {
        if (_currentRequestId)
            _send({ type: "cancel", request_id: _currentRequestId });
    }

    function newConversation() {
        sessionId = "";
        currentModel = "";
        _currentRequestId = "";
        busy = false;
        streamIndex = -1;
        messages.clear();
        _send({ type: "session_create", request_id: _uuid() });
        listSessions();
    }

    function listSessions() {
        _send({ type: "session_list", request_id: _uuid() });
    }

    // Switch to an existing session: adopt its id/model and replay its history.
    function resumeSession(id) {
        if (!id || id === sessionId)
            return;
        sessionId = id;
        busy = false;
        streamIndex = -1;
        messages.clear();
        // Pull the model from the cached session list, if present.
        for (var i = 0; i < sessions.length; i++) {
            if (sessions[i].id === id) {
                currentModel = sessions[i].model || "";
                break;
            }
        }
        _send({ type: "session_messages", request_id: _uuid(), session_id: id });
    }

    function requestModels() {
        _send({ type: "model_list", request_id: _uuid() });
    }

    // Switch the model for the CURRENT conversation via the gateway `/model`
    // slash command, routed through the desktop platform bridge (no new
    // session). The command + Hermes's confirmation appear in the transcript,
    // exactly as on Telegram.
    function setModel(id) {
        selectedModel = id;
        currentModel = id; // optimistic; the gateway confirms in its reply
        sendMessage("/model " + id);
        requestModels();
    }

    function notifyIfHidden(title, body) {
        if (popoutVisible)
            return;
        Quickshell.execDetached(["notify-send", "-a", "Roci", title, body]);
    }

    function _idleStatus() {
        return connected ? "Ready" : "Disconnected";
    }

    function _handleLine(line) {
        if (!line || !line.trim())
            return;
        var msg;
        try {
            msg = JSON.parse(line);
        } catch (e) {
            return;
        }
        switch (msg.type) {
        case "delta":
            // api_server path: incremental — append.
            if (msg.request_id === _currentRequestId && streamIndex >= 0) {
                var prev = messages.get(streamIndex).msgContent || "";
                messages.setProperty(streamIndex, "msgContent", prev + (msg.content || ""));
            }
            break;
        case "draft":
            // bridge path: growing full text — replace.
            if (msg.request_id === _currentRequestId && streamIndex >= 0)
                messages.setProperty(streamIndex, "msgContent", msg.content || "");
            break;
        case "tool_progress":
            statusText = "Running " + (msg.tool_name || "tool") + "…";
            break;
        case "chat_complete":
            if (msg.request_id === _currentRequestId) {
                if (streamIndex >= 0) {
                    var cur = messages.get(streamIndex).msgContent || "";
                    if (!cur && msg.content)
                        messages.setProperty(streamIndex, "msgContent", msg.content);
                }
                busy = false;
                statusText = _idleStatus();
                streamIndex = -1;
                _currentRequestId = "";
                listSessions(); // refresh preview/order in the switcher
            }
            break;
        case "session_created":
            sessionId = msg.session_id || "";
            currentModel = msg.model || currentModel;
            if (_pendingMessage) {
                var pending = _pendingMessage;
                _pendingMessage = "";
                _startChat(pending);
            }
            break;
        case "session_reset":
            sessionId = msg.new_id || sessionId;
            messages.append({ msgRole: "assistant", msgContent: "_Session refreshed._" });
            break;
        case "sessions":
            root.sessions = msg.data || [];
            if (_adoptOnList) {
                _adoptOnList = false;
                if (!sessionId && root.sessions.length)
                    resumeSession(_mostRecent(root.sessions));
            }
            break;
        case "messages":
            // History replay for the session we just resumed.
            if (msg.session_id === sessionId) {
                messages.clear();
                var data = msg.data || [];
                for (var i = 0; i < data.length; i++)
                    messages.append({ msgRole: data[i].role, msgContent: data[i].content || "" });
            }
            break;
        case "status":
            connected = (msg.hermes === "connected");
            daemonReady = (msg.daemon === "ready");
            if (!busy)
                statusText = _idleStatus();
            break;
        case "toast":
            notifyIfHidden(msg.title || "Roci", msg.body || "");
            break;
        case "models":
            root.models = msg.data || [];
            if (!root.selectedModel) {
                var act = root.models.filter(function (m) {
                    return m.active;
                });
                if (act.length)
                    root.selectedModel = act[0].id;
            }
            break;
        case "error":
            if (!msg.request_id || msg.request_id === _currentRequestId) {
                messages.append({ msgRole: "error", msgContent: msg.message || "error" });
                busy = false;
                statusText = _idleStatus();
                streamIndex = -1;
                _currentRequestId = "";
            }
            break;
        }
    }

    // id of the newest session by last_active (falls back to list order).
    function _mostRecent(list) {
        var best = list[0];
        for (var i = 1; i < list.length; i++) {
            if ((list[i].last_active || 0) > (best.last_active || 0))
                best = list[i];
        }
        return best.id;
    }

    // --- The persistent relay process ---
    property Process ctlProc: Process {
        command: ["hermes-dms-ctl", "stream"]
        running: true
        stdinEnabled: true

        stdout: SplitParser {
            onRead: function (line) {
                root._handleLine(line);
            }
        }
        stderr: SplitParser {
            onRead: function (line) {}
        }

        onRunningChanged: {
            if (running) {
                root.daemonReady = true;
                root.statusText = "Connecting…";
                // Broadcasts only fire on change, so pull the current state now.
                root._send({ type: "status", request_id: root._uuid() });
                root.requestModels();
                // Adopt the most-recent desktop session so history + active
                // model survive a daemon/shell restart.
                root._adoptOnList = !root.sessionId;
                root.listSessions();
            }
        }
        onExited: function (exitCode, exitStatus) {
            root.daemonReady = false;
            root.connected = false;
            root.busy = false;
            root._currentRequestId = "";
            root.streamIndex = -1;
            root.statusText = "Daemon offline";
            reconnectTimer.start();
        }
    }

    property Timer reconnectTimer: Timer {
        interval: 2000
        repeat: false
        onTriggered: if (!ctlProc.running)
            ctlProc.running = true
    }
}
