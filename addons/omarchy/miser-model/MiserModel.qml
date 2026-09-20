import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "miser.model"

  // Configuration: path to miser usage.jsonl file
  // Default: /var/lib/miser/usage.jsonl (systemd service default)
  // Override via shell.json: "miser.model": { "usageFile": "/path/to/usage.jsonl" }
  readonly property string usageFile: String(setting("usageFile", "/var/lib/miser/usage.jsonl"))

  // State
  property string model: ""
  property string tier: ""
  property string classifier: ""
  property string confidence: ""
  property int promptTokens: 0
  property int completionTokens: 0
  property int latencyMs: 0
  property bool cached: false
  property string lastRequestId: ""

  // Poll interval in milliseconds
  readonly property int pollInterval: 2000

  visible: model !== ""
  implicitWidth: visible ? Math.max(80, labelText.implicitWidth + Style.spacing.controlPaddingX * 2) : 0
  implicitHeight: barSize

  Behavior on implicitWidth {
    NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
  }

  // Process to tail the usage file
  Process {
    id: readProc
    running: false
    stdout: StdioCollector {
      id: stdCollector
      waitForEnd: true
      onStreamFinished: root.parseUsage(text)
    }
  }

  // Timer to poll the usage file
  Timer {
    id: pollTimer
    interval: root.pollInterval
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: {
      if (!readProc.running) {
        readProc.command = ["tail", "-n", "1", root.usageFile]
        readProc.running = true
      }
    }
  }

  function parseUsage(output) {
    if (!output || output.trim() === "") return;
    try {
      var record = JSON.parse(output.trim());
      
      // Only update if this is a new request
      if (record.request_id === root.lastRequestId) {
        return;
      }

      root.lastRequestId = record.request_id || "";
      root.model = record.model || "";
      root.tier = record.tier || "";
      root.classifier = record.classifier || "";
      root.confidence = record.confidence ? Number(record.confidence).toFixed(2) : "";
      root.promptTokens = record.prompt_tokens || 0;
      root.completionTokens = record.completion_tokens || 0;
      root.latencyMs = record.latency_ms || 0;
      root.cached = record.cached || false;
    } catch (e) {
      // Parse error - ignore silently
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
    switch (root.tier.toLowerCase()) {
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
        color: tierColor()
        anchors.verticalCenter: parent.verticalCenter
      }

      // Model name
      Text {
        id: labelText
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        text: formatModel(root.model)
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
      if (!root.model) return "";
      var lines = [];
      lines.push("Model: " + root.model);
      lines.push("Tier: " + root.tier);
      if (root.classifier) lines.push("Classifier: " + root.classifier);
      if (root.confidence) lines.push("Confidence: " + root.confidence);
      lines.push("Tokens: " + root.promptTokens + " → " + root.completionTokens);
      lines.push("Latency: " + root.latencyMs + "ms");
      if (root.cached) lines.push("(cached)");
      return lines.join("\n");
    }

    onEntered: if (root.bar) root.bar.showTooltip(root, tooltipText)
    onExited: if (root.bar) root.bar.hideTooltip(root)
  }
}
