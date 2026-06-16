import QtQuick
import Quickshell
import qs.Services

// Launcher plugin: typing `@<message>` sends a one-shot command to Roci.
// Fire-and-forget — the hermes-dms daemon streams the reply and delivers it as
// a desktop notification, so the result is visible whether or not the panel is
// open (the launcher closes immediately after submitting).
QtObject {
    id: root

    property var pluginService: null
    property string trigger: "@"

    signal itemsChanged

    function getItems(query) {
        const q = query ? query.trim() : "";
        if (q.length === 0) {
            return [{
                name: "Ask Roci…",
                icon: "material:smart_toy",
                comment: "Type a message to send to the desktop agent",
                action: "noop",
                categories: ["Hermes Launcher"]
            }];
        }
        return [{
            name: "Ask Roci: " + q,
            icon: "material:smart_toy",
            comment: "Send to Roci · the reply arrives as a desktop notification",
            action: "ask:" + q,
            categories: ["Hermes Launcher"],
            _preScored: 1000
        }];
    }

    function executeItem(item) {
        if (!item || !item.action)
            return;
        const idx = item.action.indexOf(":");
        if (idx < 0)
            return; // noop
        const type = item.action.substring(0, idx);
        const text = item.action.substring(idx + 1);
        if (type !== "ask" || !text)
            return;
        // argv form: no shell, no expansion of the user's message.
        Quickshell.execDetached(["hermes-dms-ctl", "chat", text]);
        showToast("Sent to Roci");
    }

    function showToast(message) {
        if (typeof ToastService !== "undefined")
            ToastService.showInfo("Roci", message);
    }
}
