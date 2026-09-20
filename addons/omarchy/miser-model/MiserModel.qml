import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "miser.model"

  // Configuration: path to miser usage.jsonl file.
  // Default matches the gateway's MISER_USAGE_FILE default. When the gateway
  // runs as your user (dev setup), override the gateway via MISER_USAGE_FILE
  // and point this at the same file, e.g.
  // shell.json: "miser.model": { "usageFile": "~/.local/state/miser/usage.jsonl" }
  readonly property string usageFile: String(setting("usageFile", "/var/lib/miser/usage.jsonl"))
  readonly property string usageDir: {
    var idx = usageFile.lastIndexOf("/")
    return idx > 0 ? usageFile.substring(0, idx) : "/"
  }

  // Latest settled UsageRecord (JSONL line) from the gateway.
  property var record: null

  // Hide the widget after this many ms without a settled auto request.
  // While the client is on miser/auto, every request refreshes the record;
  // when the user switches away (or goes idle), the widget disappears
  // instead of showing a stale routed model.
  readonly property int hideAfterMs: Number(setting("hideAfterMs", 600000))

  // The client's requested model, not the routed one: a record counts only
  // when the request came through miser/auto ("miser/auto" or "auto").
  function isAutoMode(rec) {
    if (!rec || !rec.requested_model) return false
    var parts = String(rec.requested_model).split("/")
    return parts[parts.length - 1] === "auto"
  }

  function isFresh(rec) {
    if (!rec || !rec.ts) return false
    return (Date.now() - rec.ts * 1000) < hideAfterMs
  }

  visible: record !== null && isAutoMode(record) && isFresh(record)
  implicitWidth: visible ? Math.max(80, labelText.implicitWidth + Style.spacing.controlPaddingX * 2) : 0
  implicitHeight: barSize

  Behavior on implicitWidth {
    NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
  }

  // Event-driven read of the ledger. watchChanges fires on every append;
  // reload() re-reads and onLoaded parses the last line. A missing file
  // fires onLoadFailed, which clears the record so the widget hides —
  // no stale model left behind after a gateway restart or path change.
  FileView {
    id: usageView
    path: root.usageFile
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.parse(text())
    onLoadFailed: root.record = null
  }

  // Directory watcher: FileView cannot observe a file that does not exist
  // yet (the gateway creates the ledger lazily on the first settled
  // request). Watching the parent catches ledger creation and removal.
  FileView {
    id: usageDirWatcher
    path: root.usageDir
    watchChanges: true
    printErrors: false
    onFileChanged: usageView.reload()
    onLoadFailed: root.record = null
  }

  // Fallback poll while nothing has been seen: covers a usage directory
  // created after shell startup (the dir watcher could not load it yet).
  // Once a record is parsed, inotify takes over and polling stops.
  Timer {
    interval: 5000
    running: root.record === null
    repeat: true
    triggeredOnStart: true
    onTriggered: usageView.reload()
  }

  // Freshness re-evaluation: ts-based staleness can expire with no file
  // event (client switched away or went idle), so visibility must be
  // recomputed on a clock too. No file IO — just clears an expired record.
  Timer {
    interval: 15000
    running: root.record !== null
    repeat: true
    onTriggered: if (!root.isFresh(root.record)) root.record = null
  }

  function parse(content) {
    var lines = String(content || "").split("\n").filter(function (l) {
      return l.trim() !== ""
    })
    if (lines.length === 0) {
      root.record = null
      return
    }
    try {
      var parsed = JSON.parse(lines[lines.length - 1])
      // FileView can fire onLoaded more than once for the same content —
      // dedup on request_id so the width animation does not retrigger.
      if (root.record && parsed.request_id && parsed.request_id === root.record.request_id)
        return
      root.record = parsed
    } catch (e) {
      // Malformed line — keep the previous record.
    }
  }

  // Format model name for display (shorten common prefixes)
  function formatModel(name) {
    if (!name) return "";
    // Strip provider prefix for display: "anthropic/claude-sonnet-4" -> "claude-sonnet-4"
    var parts = name.split("/");
    if (parts.length > 1) {
      return parts.slice(1).join("/");
    }
    return name;
  }

  // Tier color mapping
  function tierColor() {
    switch ((root.record ? root.record.tier : "").toLowerCase()) {
      case "trivial": return "#888888";  // gray
      case "simple": return "#4ade80";   // green
      case "standard": return "#60a5fa"; // blue
      case "hard": return "#f97316";     // orange
      case "reasoning": return "#ef4444"; // red
      default: return root.bar ? root.bar.barForeground : Color.foreground;
    }
  }

  Item {
    anchors.fill: parent
    anchors.leftMargin: Style.space(8)
    anchors.rightMargin: Style.space(8)
    clip: true

    Row {
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(4)

      // Tier indicator dot
      Rectangle {
        width: 8
        height: 8
        radius: 4
        color: root.tierColor()
        anchors.verticalCenter: parent.verticalCenter
      }

      // Model name
      Text {
        id: labelText
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        text: root.record ? root.formatModel(root.record.model) : ""
        color: root.bar ? root.bar.barForeground : Color.foreground
        font.family: root.bar ? root.bar.fontFamily : Style.font.family
        font.pixelSize: Style.font.body
        elide: Text.ElideRight
        opacity: 0.9
      }
    }
  }

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    acceptedButtons: Qt.LeftButton
    cursorShape: Qt.PointingHandCursor

    property string tooltipText: {
      if (!root.record) return "";
      var lines = [];
      lines.push("Model: " + (root.record.model || ""));
      lines.push("Tier: " + (root.record.tier || ""));
      lines.push("Tokens: " + (root.record.prompt_tokens || 0) + " → " + (root.record.completion_tokens || 0));
      lines.push("Latency: " + (root.record.latency_ms || 0) + "ms");
      if (root.record.cached) lines.push("Cached response");
      else if (root.record.cost_usd !== undefined) lines.push("Cost: $" + Number(root.record.cost_usd).toFixed(4));
      if (root.record.status !== undefined) lines.push("Status: " + root.record.status);
      return lines.join("\n");
    }

    onEntered: if (root.bar) root.bar.showTooltip(root, tooltipText)
    onExited: if (root.bar) root.bar.hideTooltip(root)
  }
}
