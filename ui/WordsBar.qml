import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui
import qs.Commons

// Words written today in omaquill, like "󰏫 312/500". The app writes
// ~/.local/state/omaquill/today.json as you type; this only reads it.
// Click to open omaquill (or bring it forward).
BarWidget {
    id: root
    moduleName: "io.github.shieldsworks.omaquill"

    readonly property string stateDir: (Quickshell.env("XDG_STATE_HOME")
        || Quickshell.env("HOME") + "/.local/state") + "/omaquill"
    property var today: null
    property string date: Qt.formatDate(new Date(), "yyyy-MM-dd")

    // Today's count only counts today; yesterday's file reads as zero.
    readonly property bool current: !!today && today.date === date
    readonly property int words: current ? Math.max(0, today.words) : 0
    readonly property int target: today ? today.target : 0
    readonly property bool reached: target > 0 && words >= target

    function group(n) {
        return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
    }

    readonly property string label: {
        let s = "󰏫 " + group(words);
        if (target > 0) s += "/" + group(target);
        return s;
    }

    FileView {
        id: file
        path: root.stateDir + "/today.json"
        watchChanges: true
        // Missing until omaquill first runs; not an error.
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try {
                root.today = JSON.parse(text());
            } catch (e) {
                root.today = null;
            }
        }
        onLoadFailed: root.today = null
    }

    // Roll over at midnight, and look again in case the file has only
    // just appeared (a watch on a missing file may not notice it).
    Timer {
        interval: 60000
        running: true
        repeat: true
        onTriggered: {
            root.date = Qt.formatDate(new Date(), "yyyy-MM-dd");
            file.reload();
        }
    }

    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    WidgetButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        text: root.label
        foreground: root.reached ? Color.accent : (root.bar ? root.bar.barForeground : Color.foreground)
        dimmed: !root.current || root.words === 0
        tooltipText: {
            if (!root.today) return "omaquill: no writing yet. Click to start.";
            let t = root.today.project + ": " + root.group(root.words) + " words today";
            if (root.target > 0)
                t += root.reached ? ", target met" : " of " + root.group(root.target);
            t += "\nManuscript " + root.group(root.today.manuscript) + " words";
            return t;
        }
        onPressed: b => {
            if (b === Qt.LeftButton)
                Quickshell.execDetached(["omaquill"]);
        }
    }
}
