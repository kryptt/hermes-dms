pragma Singleton

import QtQuick
import Quickshell
import Quickshell.Io

// Long-lived bridge to the hermes-dms daemon. Spawns `hermes-dms-ctl stream`
// (a persistent Unix-socket relay), writes JSON-lines commands to its stdin,
// and parses JSON-lines events from its stdout. Streaming deltas, tool
// progress, connection status, and toasts all arrive on this one connection.
Singleton {
    id: root

    // --- Observable state ---
    property bool connected: false     // Hermes reachable (from status events)
    property bool daemonReady: false   // the ctl stream process is alive
    property bool busy: false
    property string statusText: "Connecting…"
    property string sessionId: ""
    property bool popoutVisible: false
    property var models: []           // ollama-router catalog: [{id, loaded, active}]
    property string selectedModel: "" // "" = Hermes default

    // --- Internal ---
    property string _pendingMessage: ""
    property string _currentRequestId: ""

    // --- Signals consumed by the chat view ---
    signal userMessage(string text)
    signal assistantStarted()
    signal assistantDelta(string chunk)
    signal assistantFinished(string fullContent)
    signal errorOccurred(string message)
    signal sessionRefreshed()

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
        userMessage(text);
        busy = true;
        statusText = "Thinking…";
        if (sessionId) {
            _startChat(text);
        } else {
            // No session yet: create one, then flush this message.
            _pendingMessage = text;
            _send({ type: "session_create", request_id: _uuid(), title: "[Desktop] panel" });
        }
    }

    function _startChat(text) {
        _currentRequestId = _uuid();
        assistantStarted();
        _send({ type: "chat", request_id: _currentRequestId, session_id: sessionId, message: text });
    }

    function cancel() {
        if (_currentRequestId)
            _send({ type: "cancel", request_id: _currentRequestId });
    }

    function newConversation() {
        sessionId = "";
        _currentRequestId = "";
        busy = false;
        _send({ type: "session_create", request_id: _uuid(), title: "[Desktop] panel" });
    }

    function requestModels() {
        _send({ type: "model_list", request_id: _uuid() });
    }

    // Hermes binds the model at session creation, so a model switch just clears
    // the session — the next message starts a fresh one on the new model.
    function setModel(id) {
        selectedModel = id;
        _send({ type: "set_model", model: id });
        sessionId = "";
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
            if (msg.request_id === _currentRequestId)
                assistantDelta(msg.content || "");
            break;
        case "tool_progress":
            statusText = "Running " + (msg.tool_name || "tool") + "…";
            break;
        case "chat_complete":
            if (msg.request_id === _currentRequestId) {
                assistantFinished(msg.content || "");
                busy = false;
                statusText = _idleStatus();
                _currentRequestId = "";
            }
            break;
        case "session_created":
            sessionId = msg.session_id || "";
            if (_pendingMessage) {
                var pending = _pendingMessage;
                _pendingMessage = "";
                _startChat(pending);
            }
            break;
        case "session_reset":
            sessionId = msg.new_id || sessionId;
            sessionRefreshed();
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
                errorOccurred(msg.message || "error");
                busy = false;
                statusText = _idleStatus();
                _currentRequestId = "";
            }
            break;
        }
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
                // Ask for the current status now (broadcasts only fire on change).
                root._send({ type: "status", request_id: root._uuid() });
                root.requestModels();
            }
        }
        onExited: function (exitCode, exitStatus) {
            root.daemonReady = false;
            root.connected = false;
            root.busy = false;
            root._currentRequestId = "";
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
