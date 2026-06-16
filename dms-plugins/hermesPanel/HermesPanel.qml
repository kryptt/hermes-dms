import QtQuick
import Quickshell.Io
import qs.Common
import qs.Widgets
import qs.Modules.Plugins

PluginComponent {
    id: root

    layerNamespacePlugin: "hermesPanel"

    // Use DMS's managed popout: it anchors to the bar pill (above a bottom bar,
    // below a top bar), follows the focused monitor, and handles keyboard focus
    // + click-away/Escape close. Sized to the chat.
    popoutWidth: 660
    popoutHeight: 740

    // Keyboard shortcut: bind a key to `dms ipc call hermesPanel toggle`.
    // triggerPopout() positions the popout at the pill exactly like a click.
    IpcHandler {
        function toggle(): string {
            root.triggerPopout();
            return "toggled";
        }
        target: "hermesPanel"
    }

    popoutContent: Component {
        Item {
            id: popoutRoot

            // PluginPopout injects a close function here once the content loads.
            property var closePopout

            implicitWidth: 660
            implicitHeight: 740

            // Best-effort: lets HermesService suppress duplicate desktop
            // notifications while the panel is on screen.
            onVisibleChanged: HermesService.popoutVisible = visible

            // Translucent backdrop so the chat stays legible over whatever's
            // behind it (e.g. a terminal). DMS popout convention
            // (surfaceContainer at the user's popupTransparency setting).
            Rectangle {
                anchors.fill: parent
                radius: 20
                color: Theme.panelBackground()
                border.width: 1
                border.color: Theme.outlineVariant
            }

            HermesPanelChat {
                anchors.fill: parent
                onEscapePressed: if (popoutRoot.closePopout)
                    popoutRoot.closePopout()
            }
        }
    }

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS
            DankIcon {
                name: HermesService.busy ? "hourglass_top" : "smart_toy"
                color: HermesService.busy ? "#FF9800" : (HermesService.connected ? Theme.primary : Theme.surfaceVariantText)
                size: root.iconSize
                anchors.verticalCenter: parent.verticalCenter
            }
            StyledText {
                anchors.verticalCenter: parent.verticalCenter
                text: HermesService.busy ? "Roci…" : "Roci"
                color: Theme.surfaceText
                font.pixelSize: Theme.fontSizeSmall
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: 2
            DankIcon {
                name: HermesService.busy ? "hourglass_top" : "smart_toy"
                color: HermesService.busy ? "#FF9800" : (HermesService.connected ? Theme.primary : Theme.surfaceVariantText)
                size: root.iconSize
                anchors.horizontalCenter: parent.horizontalCenter
            }
        }
    }
}
