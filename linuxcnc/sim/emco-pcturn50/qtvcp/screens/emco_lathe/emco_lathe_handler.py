"""emco_lathe — a QtVCP screen for the EMCO PC Turn 50 training simulator.

The screen exists to teach the routine in docs/40-operator-guide.md, and to
enforce the parts of it that the real machine will punish:

  * reference after every power-on AND every E-stop        (doc 40 §5)
  * tell the control which belt is fitted; stay in range   (doc 40 §6)
  * touch off the work and each tool, then verify          (doc 40 §9)
  * prove every program, again after every edit            (doc 40 §10)

Every decision is made in emco_rules.py, which has no Qt or LinuxCNC in it
and is unit-tested on its own. This file is the glue: it builds the widgets,
feeds the rules what LinuxCNC reports, and acts on the answers.

The layout is built here in Python rather than in Qt Designer, so it can be
read and reviewed as code. Widgets made this way are not HAL-initialised by
QtVCP (that happens before initialized__ runs, and LinuxCNC 2.9 has no
pre_hal_init__ hook), so _hal_init_children() does it by hand.
"""

import os
import sys

import linuxcnc
from PyQt5 import QtCore, QtGui, QtWidgets

from qtvcp.core import Action, Info, Status
from qtvcp import logger
from qtvcp.widgets.widget_baseclass import _HalWidgetBase
from qtvcp.widgets.action_button import ActionButton
from qtvcp.widgets.dro_widget import DROLabel
from qtvcp.widgets.gcode_graphics import GCodeGraphics
from qtvcp.widgets.gcode_editor import GcodeDisplay
from qtvcp.widgets.mdi_line import MDILine
from qtvcp.widgets.status_label import StatusLabel
from qtvcp.widgets.tool_offsetview import ToolOffsetView
from qtvcp.widgets.origin_offsetview import OriginOffsetView

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import emco_rules as R  # noqa: E402

LOG = logger.getLogger(__name__)
STATUS = Status()
ACTION = Action()
INFO = Info()

# Single-block first runs start slow (doc 40 §10.3: "feed override low").
FIRST_RUN_FEED_OVERRIDE = 25
JOG_INCREMENTS = [("Continuous", 0.0), ("1 mm", 1.0), ("0.1 mm", 0.1), ("0.01 mm", 0.01)]
THREAD_GCODES = (330, 331, 760)  # G33, G33.1, G76 as stat.gcodes reports them


# =============================================================================
# The touch-off wizard — the steps are doc 40 §9, nearly word for word
# =============================================================================
# Each step: (title, instructions, button label or None, action key or None).
# Keep the wording in step with docs/40-operator-guide.md; if one changes, so
# does the other.
WORK_Z_STEPS = [
    ("Set the work offset (Z) — 1/5",
     "Reference the machine and chuck your stock.\n\n"
     "Select a turning tool (T1) and start the spindle at a moderate speed "
     "for the material, e.g. M3 S800 for small aluminium.", None, None),
    ("Face the end — 2/5",
     "Jog the tool carefully to just touch the rotating face, then take a "
     "light facing cut so the face is clean and square.", None, None),
    ("Do not move Z — 3/5",
     "The tool tip is now exactly at your part's Z0.\n\n"
     "Press the button to make this G54 Z0.", "Set G54 Z0 here", "work_z"),
    ("Retract and stop — 4/5",
     "Retract in X (G53 G0 X48 in radius terms — the button does it), "
     "then stop the spindle.", "Retract X and stop spindle", "retract"),
    ("Verify — 5/5",
     "Commanding G0 Z5 should put the tool 5 mm to the right of the fresh "
     "face. If it doesn't, stop and redo — do not adjust as you go.",
     "Move to Z5", "verify_z"),
]

TOOL_STEPS = [
    ("Tool offsets — 1/5",
     "With the work offset set, select the tool to measure (T_ M6 in MDI, "
     "or it is the tool already in position).", None, None),
    ("X by cutting — 2/5",
     "Take a light skim cut along the diameter. Retract in Z ONLY — do not "
     "move X. Stop the spindle and measure the diameter with a micrometer "
     "or calliper. Enter it below.", "Set tool X from measured diameter", "tool_x"),
    ("Z by touching — 3/5",
     "Touch the tool tip to the freshly faced end (paper drag or a light "
     "scratch).", "Set tool Z0 here", "tool_z"),
    ("Compensation — 4/5",
     "If you will use cutter compensation (G41/G42), check the nose radius "
     "and orientation in the Tools tab. Otherwise leave it off (G40).", None, None),
    ("Verify — 5/5",
     "With a 20 mm bar in the chuck, G0 X20 Z5 must put the tool visibly "
     "at 20 mm diameter, 5 mm off the face. It costs seconds and catches "
     "decade errors.", "Move to X20 (dia) Z5", "verify_tool"),
]


class HandlerClass:
    def __init__(self, halcomp, widgets, paths):
        self.hal = halcomp
        self.w = widgets
        self.PATH = paths
        self.cmd = linuxcnc.command()

        self.gate = R.ReferenceGate()
        self.gate_reason = "Not referenced"
        self.proven = R.ProvenStore()
        self.machine = self._machine_from_ini()
        self.settings = QtCore.QSettings("emco-lathe", "emco_lathe")
        self.belt = int(self.settings.value("belt", R.DEFAULT_BELT))
        if self.belt not in R.BELT_RANGES:
            self.belt = R.DEFAULT_BELT

        self.program_path = None
        self.program_text = ""
        self.findings = []
        self.preflight_override = False
        self.running_program = False
        self.run_clean = False
        self.unhome_sent = False
        self.was_on = False
        self.last_violation = None

    @staticmethod
    def _machine_from_ini():
        ini = INFO.INI
        return R.Machine(
            x_retract=float(ini.find("JOINT_0", "HOME") or 0),
            z_park=float(ini.find("JOINT_1", "HOME") or 0),
            tool_change_retracts=bool(ini.find("EMCIO", "TOOL_CHANGE_POSITION")),
        )

    # -------------------------------------------------------------------------
    # QtVCP entry points
    # -------------------------------------------------------------------------
    def initialized__(self):
        self.w.setWindowTitle("EMCO PC Turn 50 — simulator")
        self._build()
        self._hal_init_children()

        STATUS.connect("periodic", lambda w: self._periodic())
        STATUS.connect("file-loaded", lambda w, f: self._file_loaded(f))
        STATUS.connect("error", lambda w, kind, text: self._error(kind, text))
        STATUS.connect("interp-idle", lambda w: self._interp_idle())
        STATUS.connect("state-estop", lambda w: self._run_spoiled("E-stop"))
        STATUS.connect("diameter-mode", lambda w, d: self._diameter_mode(d))
        STATUS.connect("tool-in-spindle-changed", lambda w, t: self.tool_label.setText("T{}".format(t)))

        self._set_belt(self.belt, announce=False)
        self._refresh_controls()

    def before_loop__(self):
        # QtVCP does not honour [DISPLAY]OPEN_FILE, so do it here.
        open_file = INFO.INI.find("DISPLAY", "OPEN_FILE")
        if open_file:
            path = os.path.abspath(os.path.join(os.path.dirname(INFO.INIPATH), open_file))
            if os.path.exists(path):
                QtCore.QTimer.singleShot(500, lambda: ACTION.OPEN_PROGRAM(path))
        QtCore.QTimer.singleShot(0, self.w.showMaximized)

    def processed_key_event__(self, receiver, event, is_pressed, key, code, shift, cntrl):
        # F1 / F2 as in AXIS, so doc 70's "F1, F2" still works. Every other
        # key goes to whichever widget has focus.
        if is_pressed and code == QtCore.Qt.Key_F1:
            ACTION.SET_ESTOP_STATE(not STATUS.estop_is_clear())
            return True
        if is_pressed and code == QtCore.Qt.Key_F2:
            ACTION.SET_MACHINE_STATE(not STATUS.machine_is_on())
            return True
        return False

    def processed_focus_event__(self, receiver, event):
        pass

    # -------------------------------------------------------------------------
    # Building the screen
    # -------------------------------------------------------------------------
    def _build(self):
        central = QtWidgets.QWidget()
        central.setObjectName("central")
        self.w.setCentralWidget(central)
        root = QtWidgets.QHBoxLayout(central)

        # Left: the backplot over a tab set — program text first, then the
        # pages you visit rather than watch.
        left = QtWidgets.QSplitter(QtCore.Qt.Vertical)
        self.graphics = GCodeGraphics()
        self.graphics.setObjectName("gcode_graphics")
        # The XZ plane seen from above the cross slide, as stock qtlathe does.
        self.graphics.setProperty("_view", "y")
        self.graphics.setMinimumSize(480, 260)
        left.addWidget(self.graphics)
        self.tabs = QtWidgets.QTabWidget()
        self.program_view = GcodeDisplay()
        self.program_view.setObjectName("gcode_display")
        self.tabs.addTab(self.program_view, "Program")
        self.tabs.addTab(self._preflight_tab(), "Preflight")
        self.tabs.addTab(self._mdi_tab(), "MDI")
        self.tabs.addTab(self._touchoff_tab(), "Touch-off")
        tools = ToolOffsetView()
        tools.setObjectName("tool_offsetview")
        self.tabs.addTab(tools, "Tools")
        offsets = OriginOffsetView()
        offsets.setObjectName("origin_offsetview")
        self.tabs.addTab(offsets, "Offsets")
        left.addWidget(self.tabs)
        left.setSizes([470, 380])
        root.addWidget(left, 3)

        # Right: everything an operator watches while the machine moves. It
        # scrolls rather than pushing the window off a small screen (the
        # container's display is 1280 x 900).
        column = QtWidgets.QWidget()
        right = QtWidgets.QVBoxLayout(column)
        right.setContentsMargins(0, 0, 0, 0)
        scroll = QtWidgets.QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QtWidgets.QFrame.NoFrame)
        scroll.setHorizontalScrollBarPolicy(QtCore.Qt.ScrollBarAlwaysOff)
        scroll.setWidget(column)
        scroll.setMinimumWidth(500)
        root.addWidget(scroll, 2)
        right.addWidget(self._machine_strip())
        right.addWidget(self._dro_box())
        right.addWidget(self._spindle_box())
        right.addWidget(self._jog_box())
        right.addWidget(self._run_box())

        self.messages = QtWidgets.QPlainTextEdit()
        self.messages.setObjectName("messages")
        self.messages.setReadOnly(True)
        self.messages.setMaximumBlockCount(200)
        self.messages.setMinimumHeight(70)
        right.addWidget(self.messages, 1)

    def _machine_strip(self):
        box = QtWidgets.QFrame()
        lay = QtWidgets.QGridLayout(box)
        estop = ActionButton()
        estop.setText("E-STOP")
        estop.setObjectName("estop_button")
        estop.set_estop(True)
        estop.setCheckable(True)
        power = ActionButton()
        power.setText("Machine on")
        power.setObjectName("power_button")
        power.set_machine_on(True)
        power.setCheckable(True)
        lay.addWidget(estop, 0, 0)
        lay.addWidget(power, 0, 1)

        self.banner = QtWidgets.QLabel()
        self.banner.setObjectName("reference_banner")
        self.banner.setAlignment(QtCore.Qt.AlignCenter)
        self.banner.setWordWrap(True)
        lay.addWidget(self.banner, 1, 0, 1, 3)
        self.home_button = QtWidgets.QPushButton("Home All")
        self.home_button.setObjectName("home_button")
        self.home_button.clicked.connect(self._home_all)
        lay.addWidget(self.home_button, 0, 2)
        return box

    def _dro(self, name, axis_index, reference, follow_g7=True):
        d = DROLabel()
        d.setObjectName(name)
        d.joint_number = axis_index
        d.reference_type = reference
        d.follow_m7m8_mode = follow_g7
        d.set_to_mm()
        d.metric_text_template = "%9.3f"
        d.allow_reference_change_requests = False
        return d

    def _dro_box(self):
        box = QtWidgets.QGroupBox("Position (work / machine)")
        g = QtWidgets.QGridLayout(box)
        self.x_label = QtWidgets.QLabel("X Ø")
        self.x_label.setObjectName("dro_axis")
        z_label = QtWidgets.QLabel("Z")
        z_label.setObjectName("dro_axis")
        g.addWidget(self.x_label, 0, 0)
        g.addWidget(self._dro("dro_x_rel", 0, 1), 0, 1)
        g.addWidget(self._dro("dro_x_abs", 0, 0), 0, 2)
        g.addWidget(z_label, 2, 0)
        g.addWidget(self._dro("dro_z_rel", 2, 1), 2, 1)
        g.addWidget(self._dro("dro_z_abs", 2, 0), 2, 2)
        # Radius beneath the diameter: the machine moves in radius, drawings
        # are in diameter, and confusing the two is doc 50 §2's whole point.
        r = QtWidgets.QLabel("X radius")
        r.setObjectName("dro_small")
        g.addWidget(r, 1, 0)
        rad = self._dro("dro_x_radius", 0, 1, follow_g7=False)
        rad.setProperty("small", True)
        g.addWidget(rad, 1, 1)
        g.addWidget(QtWidgets.QLabel("Tool"), 3, 0)
        self.tool_label = QtWidgets.QLabel("T0")
        self.tool_label.setObjectName("tool_label")
        g.addWidget(self.tool_label, 3, 1)
        state = StatusLabel()
        state.setObjectName("gcodes_label")
        state.set_gcodes(True)
        state.setWordWrap(True)
        g.addWidget(state, 4, 0, 1, 3)
        return box

    def _spindle_box(self):
        box = QtWidgets.QGroupBox("Belt and spindle")
        lay = QtWidgets.QGridLayout(box)
        self.belt_buttons = QtWidgets.QButtonGroup(box)
        for belt, (lo, hi) in sorted(R.BELT_RANGES.items()):
            b = QtWidgets.QRadioButton("Belt {}\n{}–{}".format(belt, lo, hi))
            self.belt_buttons.addButton(b, belt)
            lay.addWidget(b, 0, belt - 1)
        self.belt_buttons.buttonClicked[int].connect(lambda b: self._set_belt(b, announce=True))

        self.rpm = QtWidgets.QSpinBox()
        self.rpm.setSuffix(" rpm")
        self.rpm.setSingleStep(50)
        self.rpm.setRange(0, max(hi for _, hi in R.BELT_RANGES.values()))
        self.rpm.setValue(800)  # doc 40 §9.1's "moderate speed" for small aluminium
        lay.addWidget(self.rpm, 1, 0)
        self.fwd = QtWidgets.QPushButton("M3 FWD")
        self.fwd.clicked.connect(lambda: self._spindle(1))
        self.rev = QtWidgets.QPushButton("M4 REV")
        self.rev.clicked.connect(lambda: self._spindle(-1))
        stop = QtWidgets.QPushButton("M5 STOP")
        stop.clicked.connect(lambda: ACTION.SET_SPINDLE_STOP())
        lay.addWidget(self.fwd, 1, 1)
        lay.addWidget(self.rev, 1, 2)
        lay.addWidget(stop, 2, 1, 1, 2)
        actual = StatusLabel()
        actual.setObjectName("spindle_actual")
        actual.set_requested_spindle_speed(True)
        actual.set_textTemplate("S %d rpm")
        lay.addWidget(actual, 2, 0)
        return box

    def _jog_box(self):
        box = QtWidgets.QGroupBox("Jog")
        lay = QtWidgets.QGridLayout(box)
        self.jog_buttons = []
        # On a lathe X- moves toward the centreline, Z- toward the chuck.
        for col, (text, axis, direction) in enumerate(
                [("X−", "X", -1), ("X+", "X", 1), ("Z−", "Z", -1), ("Z+", "Z", 1)]):
            b = QtWidgets.QPushButton(text)
            b.setAutoRepeat(False)
            b.pressed.connect(lambda a=axis, d=direction: self._jog(a, d))
            b.released.connect(lambda a=axis: self._jog(a, 0))
            lay.addWidget(b, 0, col)
            self.jog_buttons.append(b)
        self.jog_incr = QtWidgets.QComboBox()
        for text, value in JOG_INCREMENTS:
            self.jog_incr.addItem(text, value)
        lay.addWidget(self.jog_incr, 1, 0, 1, 2)
        self.jog_rate = QtWidgets.QDoubleSpinBox()
        self.jog_rate.setSuffix(" mm/s")
        self.jog_rate.setDecimals(2)
        max_v = float(INFO.INI.find("TRAJ", "MAX_LINEAR_VELOCITY") or 10)
        self.jog_rate.setRange(0.05, max_v)
        self.jog_rate.setValue(float(INFO.INI.find("DISPLAY", "DEFAULT_LINEAR_VELOCITY") or 1))
        lay.addWidget(self.jog_rate, 1, 2, 1, 2)
        return box

    def _run_box(self):
        page = QtWidgets.QGroupBox("Program")
        lay = QtWidgets.QVBoxLayout(page)
        row = QtWidgets.QHBoxLayout()
        load = QtWidgets.QPushButton("Open…")
        load.clicked.connect(self._open_dialog)
        row.addWidget(load)
        self.start_button = QtWidgets.QPushButton("Cycle start")
        self.start_button.setObjectName("cycle_start")
        self.start_button.clicked.connect(self._cycle_start)
        row.addWidget(self.start_button)
        pause = QtWidgets.QPushButton("Pause / resume")
        pause.clicked.connect(lambda: ACTION.PAUSE())
        row.addWidget(pause)
        stop = QtWidgets.QPushButton("Stop")
        stop.setObjectName("cycle_stop")
        stop.clicked.connect(self._abort)
        row.addWidget(stop)
        lay.addLayout(row)

        row = QtWidgets.QHBoxLayout()
        self.single_block = QtWidgets.QCheckBox("Single block")
        row.addWidget(self.single_block)
        row.addWidget(QtWidgets.QLabel("Feed override"))
        self.feed = QtWidgets.QSlider(QtCore.Qt.Horizontal)
        max_fo = int(float(INFO.INI.find("DISPLAY", "MAX_FEED_OVERRIDE") or 1) * 100)
        self.feed.setRange(0, max_fo)
        self.feed.setValue(100)
        self.feed.valueChanged.connect(lambda v: ACTION.SET_FEED_RATE(v))
        self.feed_label = QtWidgets.QLabel("100%")
        self.feed.valueChanged.connect(lambda v: self.feed_label.setText("{}%".format(v)))
        row.addWidget(self.feed, 1)
        row.addWidget(self.feed_label)
        lay.addLayout(row)

        self.proven_label = QtWidgets.QLabel()
        self.proven_label.setObjectName("proven_label")
        self.proven_label.setWordWrap(True)
        lay.addWidget(self.proven_label)
        return page

    def _preflight_tab(self):
        page = QtWidgets.QWidget()
        lay = QtWidgets.QVBoxLayout(page)
        self.report = QtWidgets.QListWidget()
        self.report.setObjectName("preflight")
        self.report.setWordWrap(True)
        self.report.itemActivated.connect(self._goto_finding)
        lay.addWidget(self.report, 1)
        self.override = QtWidgets.QCheckBox("Run despite errors (training only — it is logged)")
        self.override.toggled.connect(self._override_toggled)
        lay.addWidget(self.override)
        return page

    def _mdi_tab(self):
        page = QtWidgets.QWidget()
        lay = QtWidgets.QVBoxLayout(page)
        self.mdi = MDILine()
        self.mdi.setObjectName("mdi_line")
        lay.addWidget(self.mdi)
        hint = QtWidgets.QLabel(
            "Type a line and press Enter. Remember G7: in diameter mode the full "
            "X retract is G53 G0 X{:g}, not X{:g}.".format(
                self.machine.x_retract * 2, self.machine.x_retract))
        hint.setWordWrap(True)
        lay.addWidget(hint)
        lay.addStretch(1)
        return page

    def _touchoff_tab(self):
        page = QtWidgets.QWidget()
        lay = QtWidgets.QVBoxLayout(page)
        pick = QtWidgets.QHBoxLayout()
        work = QtWidgets.QPushButton("Work Z0 (§9.1)")
        tool = QtWidgets.QPushButton("Tool offsets (§9.2)")
        pick.addWidget(work)
        pick.addWidget(tool)
        lay.addLayout(pick)

        self.wizard_title = QtWidgets.QLabel()
        self.wizard_title.setObjectName("wizard_title")
        self.wizard_text = QtWidgets.QLabel()
        self.wizard_text.setWordWrap(True)
        self.wizard_text.setAlignment(QtCore.Qt.AlignTop)
        lay.addWidget(self.wizard_title)
        lay.addWidget(self.wizard_text, 1)

        self.diameter_entry = QtWidgets.QDoubleSpinBox()
        self.diameter_entry.setDecimals(3)
        self.diameter_entry.setRange(0.0, 2 * self.machine.x_retract)
        self.diameter_entry.setPrefix("Measured Ø ")
        self.diameter_entry.setSuffix(" mm")
        lay.addWidget(self.diameter_entry)
        self.wizard_action = QtWidgets.QPushButton()
        self.wizard_action.clicked.connect(self._wizard_do)
        lay.addWidget(self.wizard_action)
        nav = QtWidgets.QHBoxLayout()
        self.wizard_back = QtWidgets.QPushButton("◀ Back")
        self.wizard_next = QtWidgets.QPushButton("Next ▶")
        self.wizard_back.clicked.connect(lambda: self._wizard_show(self.wizard_step - 1))
        self.wizard_next.clicked.connect(lambda: self._wizard_show(self.wizard_step + 1))
        nav.addWidget(self.wizard_back)
        nav.addWidget(self.wizard_next)
        lay.addLayout(nav)

        work.clicked.connect(lambda: self._wizard_start(WORK_Z_STEPS))
        tool.clicked.connect(lambda: self._wizard_start(TOOL_STEPS))
        self._wizard_start(WORK_Z_STEPS)
        return page

    def _hal_init_children(self):
        # The .ui file holds no HAL widgets, so every one found here was made
        # by _build() and has not been initialised yet.
        for widget in self.w.findChildren(QtCore.QObject):
            if isinstance(widget, _HalWidgetBase):
                widget.hal_init()

    # -------------------------------------------------------------------------
    # The periodic poll: errors, the reference gate, the spindle watch
    # -------------------------------------------------------------------------
    def _periodic(self):
        # Nothing else polls the error channel: that is ScreenOptions' job in
        # stock screens, and this one does not use it.
        try:
            e = STATUS.poll_error()
            if e:
                STATUS.emit("error", e[0], e[1])
        except Exception as exc:  # noqa: BLE001 — keep the poll alive
            LOG.error("error channel: {}".format(exc))

        stat = STATUS.stat
        estop = not STATUS.estop_is_clear()
        on = STATUS.machine_is_on()
        homed = [bool(stat.joint[j]["homed"]) for j in range(stat.joints)]
        reason = self.gate.update(estop, on, homed)

        # LinuxCNC keeps its homed flags across an E-stop; the real machine
        # does not keep its position. Make LinuxCNC agree with the machine.
        if on and self.gate.position_lost and all(homed) and not self.unhome_sent \
                and STATUS.is_interp_idle():
            self.unhome_sent = True
            self._log("Position lost at E-stop — un-referencing. Home All to continue.")
            ACTION.SET_MACHINE_UNHOMED(-1)
        if not self.gate.position_lost:
            self.unhome_sent = False

        if on and not self.was_on:
            QtCore.QTimer.singleShot(0, self._ask_belt)
        self.was_on = on

        if reason != self.gate_reason:
            self.gate_reason = reason
            self._refresh_controls()

        self._watch_spindle(stat)

    def _watch_spindle(self, stat):
        rpm = stat.spindle[0]["speed"] * stat.spindle[0]["override"]
        threading = stat.gcodes[1] in THREAD_GCODES
        violation = R.spindle_violation(self.belt, rpm, threading)
        if violation and violation != self.last_violation:
            self._log("SPINDLE: " + violation)
            if STATUS.is_auto_running() and not stat.paused:
                ACTION.PAUSE()
            elif not STATUS.is_auto_running():
                ACTION.SET_SPINDLE_STOP()
        self.last_violation = violation

    def _refresh_controls(self):
        ok = self.gate_reason is None
        self.banner.setText("REFERENCED — ready" if ok else self.gate_reason)
        self.banner.setProperty("ok", ok)
        self.banner.style().unpolish(self.banner)
        self.banner.style().polish(self.banner)
        on = STATUS.machine_is_on()
        self.home_button.setEnabled(on)
        for w in [self.start_button, self.mdi, self.fwd, self.rev, self.wizard_action] + self.jog_buttons:
            w.setEnabled(ok)
        self._refresh_run_state()

    # -------------------------------------------------------------------------
    # Referencing and belt
    # -------------------------------------------------------------------------
    def _home_all(self):
        self._log("Referencing: X drives out to X{:g} (radius), Z to Z{:g}. Keep the path clear.".format(
            self.machine.x_retract, self.machine.z_park))
        ACTION.SET_MACHINE_HOMING(-1)

    def _ask_belt(self):
        items = ["Belt {}  ({}–{} rpm)".format(b, lo, hi) for b, (lo, hi) in sorted(R.BELT_RANGES.items())]
        choice, ok = QtWidgets.QInputDialog.getItem(
            self.w, "Which belt is fitted?",
            "Tell the control which belt position is fitted (doc 40 §6).\n"
            "The real control uses it for speed monitoring and threading.",
            items, self.belt - 1, False)
        if ok:
            self._set_belt(items.index(choice) + 1, announce=True)

    def _set_belt(self, belt, announce):
        self.belt = belt
        self.settings.setValue("belt", belt)
        self.belt_buttons.button(belt).setChecked(True)
        lo, hi = R.belt_range(belt)
        self.rpm.setRange(lo, hi)
        self.rpm.setValue(R.clamp_rpm(belt, self.rpm.value()))
        if announce:
            self._log("Belt {} fitted: {}–{} rpm.".format(belt, lo, hi))
        if self.program_text:
            self._run_preflight()

    def _spindle(self, direction):
        ACTION.SET_MANUAL_MODE()
        ACTION.SET_SPINDLE_ROTATION(direction, R.clamp_rpm(self.belt, self.rpm.value()), 0)

    # -------------------------------------------------------------------------
    # Jogging
    # -------------------------------------------------------------------------
    def _jog(self, axis, direction):
        if not STATUS.machine_is_on():
            return
        ACTION.SET_MANUAL_MODE()
        # Teleop jogs by axis index (X0 Y1 Z2); free mode by joint (X0 Z1).
        if STATUS.stat.motion_mode == linuxcnc.TRAJ_MODE_FREE:
            index = {"X": 0, "Z": 1}[axis]
        else:
            index = {"X": 0, "Z": 2}[axis]
        distance = self.jog_incr.currentData()
        if direction == 0:
            if distance == 0:
                ACTION.JOG(index, 0, 0)
            return
        ACTION.JOG(index, direction, self.jog_rate.value(), distance)

    # -------------------------------------------------------------------------
    # Programs: load, preflight, prove
    # -------------------------------------------------------------------------
    def _open_dialog(self):
        prefix = INFO.INI.find("DISPLAY", "PROGRAM_PREFIX") or "~"
        start = os.path.abspath(os.path.join(os.path.dirname(INFO.INIPATH), os.path.expanduser(prefix)))
        path, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.w, "Open program", start, "G-code (*.ngc *.nc *.tap);;All files (*)")
        if path:
            ACTION.OPEN_PROGRAM(path)

    def _file_loaded(self, path):
        self.program_path = path
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                self.program_text = f.read()
        except OSError as exc:
            self.program_text = ""
            self._log("Could not read {}: {}".format(path, exc))
        self.override.setChecked(False)
        self._log("Loaded {}".format(os.path.basename(path)))
        self._run_preflight()

    def _run_preflight(self):
        self.findings = R.preflight(self.program_text, self.belt, self.machine)
        self.report.clear()
        if not self.findings:
            self.report.addItem("Preflight: nothing found. Still read the header and check your setup.")
        for f in self.findings:
            item = QtWidgets.QListWidgetItem("{} line {}: {}".format(f.severity.upper(), f.line, f.message))
            item.setData(QtCore.Qt.UserRole, f.line)
            item.setForeground(QtGui.QColor("#c62828" if f.severity == R.ERROR else "#b26a00"))
            self.report.addItem(item)
        errors = sum(f.severity == R.ERROR for f in self.findings)
        index = self.tabs.indexOf(self.report.parentWidget())
        self.tabs.setTabText(index, "Preflight ({})".format(len(self.findings)) if self.findings else "Preflight")
        if errors:
            self.tabs.setCurrentIndex(index)
        self._refresh_run_state()

    def _goto_finding(self, item):
        line = item.data(QtCore.Qt.UserRole)
        if line:
            self.tabs.setCurrentWidget(self.program_view)
            self.program_view.setCursorPosition(line - 1, 0)
            self.program_view.ensureLineVisible(line - 1)

    def _has_errors(self):
        return any(f.severity == R.ERROR for f in self.findings)

    def _is_proven(self):
        return bool(self.program_text) and self.proven.is_proven(self.program_text)

    def _refresh_run_state(self):
        if not self.program_text:
            self.proven_label.setText("No program loaded.")
            return
        name = os.path.basename(self.program_path or "")
        if self._is_proven():
            self.proven_label.setText("{} — PROVEN in this simulator.".format(name))
            self.single_block.setEnabled(True)
        else:
            self.proven_label.setText(
                "{} — NOT PROVEN. The first run is single block at {}% feed; "
                "a clean finish marks it proven. Any edit un-proves it (doc 40 §10).".format(
                    name, FIRST_RUN_FEED_OVERRIDE))
            self.single_block.setChecked(True)
            self.single_block.setEnabled(False)
        blocked = self._has_errors() and not self.preflight_override
        self.start_button.setEnabled(self.gate_reason is None and not blocked)

    def _override_toggled(self, checked):
        self.preflight_override = checked
        if checked:
            self._log("Preflight override ON for {} — running a program with known errors.".format(
                os.path.basename(self.program_path or "?")))
        self._refresh_run_state()

    def _cycle_start(self):
        if self.gate_reason is not None:
            self._log("Cycle start refused: " + self.gate_reason)
            return
        if self._has_errors() and not self.preflight_override:
            self._log("Cycle start refused: the preflight found errors.")
            return
        stat = STATUS.stat
        idle = stat.interp_state == linuxcnc.INTERP_IDLE
        if idle:
            if not self._is_proven():
                self.feed.setValue(FIRST_RUN_FEED_OVERRIDE)
            self.running_program = True
            self.run_clean = not self.preflight_override
        ACTION.SET_AUTO_MODE()
        if self.single_block.isChecked():
            if STATUS.is_auto_running() and not stat.paused:
                self.cmd.auto(linuxcnc.AUTO_PAUSE)
            self.cmd.auto(linuxcnc.AUTO_STEP)
        elif stat.paused:
            self.cmd.auto(linuxcnc.AUTO_RESUME)
        else:
            ACTION.RUN(0)

    def _abort(self):
        self._run_spoiled("stopped by operator")
        ACTION.ABORT()

    def _run_spoiled(self, why):
        if self.running_program and self.run_clean:
            self._log("This run will not prove the program ({}).".format(why))
        self.run_clean = False

    def _error(self, kind, text):
        self._log(text.strip())
        if kind in (linuxcnc.NML_ERROR, linuxcnc.OPERATOR_ERROR):
            self._run_spoiled("an error was reported")

    def _interp_idle(self):
        if not self.running_program:
            return
        self.running_program = False
        # An error can arrive in the same instant the interpreter goes idle
        # (sim/acceptance-test.py hit exactly this), so let it land first.
        QtCore.QTimer.singleShot(700, self._finish_run)

    def _finish_run(self):
        try:
            e = STATUS.poll_error()
            if e:
                STATUS.emit("error", e[0], e[1])
        except Exception:  # noqa: BLE001
            pass
        if self.run_clean and self.program_text and not self._is_proven():
            self.proven.mark_proven(self.program_text, os.path.basename(self.program_path or ""))
            self._log("Program proven. Next run may go at full feed, without single block.")
        self._refresh_run_state()

    # -------------------------------------------------------------------------
    # Touch-off wizard
    # -------------------------------------------------------------------------
    def _wizard_start(self, steps):
        self.wizard_steps = steps
        self._wizard_show(0)

    def _wizard_show(self, index):
        index = max(0, min(index, len(self.wizard_steps) - 1))
        self.wizard_step = index
        title, text, button, action = self.wizard_steps[index]
        self.wizard_title.setText(title)
        self.wizard_text.setText(text)
        self.wizard_action.setVisible(button is not None)
        self.wizard_action.setText(button or "")
        self.diameter_entry.setVisible(action == "tool_x")
        self.wizard_back.setEnabled(index > 0)
        self.wizard_next.setEnabled(index < len(self.wizard_steps) - 1)

    def _wizard_do(self):
        action = self.wizard_steps[self.wizard_step][3]
        tool = STATUS.stat.tool_in_spindle
        diameter_mode = self._in_diameter_mode()
        try:
            if action == "work_z":
                cmds = R.touchoff_work_z()
            elif action == "retract":
                cmds = ["M5", "G53 G0 X{:g}".format(self.machine.x_retract * (2 if diameter_mode else 1))]
            elif action == "verify_z":
                cmds = R.verify_work_z()
            elif action in ("tool_x", "tool_z"):
                if tool <= 0:
                    self._log("No tool in position — select one first (T_ M6).")
                    return
                if action == "tool_x":
                    cmds = R.touchoff_tool_x(tool, self.diameter_entry.value(), diameter_mode)
                else:
                    cmds = R.touchoff_tool_z(tool)
                # A changed table entry only takes effect once re-applied.
                cmds.append("G43")
            elif action == "verify_tool":
                cmds = R.verify_tool(diameter_mode)
            else:
                return
        except ValueError as exc:
            self._log("Touch-off: {}".format(exc))
            return
        self._log("Touch-off: " + " / ".join(cmds))
        if ACTION.CALL_MDI_WAIT("\n".join(cmds), 30) == 0:
            self._wizard_show(self.wizard_step + 1)
        ACTION.SET_MANUAL_MODE()

    # -------------------------------------------------------------------------
    # Small helpers
    # -------------------------------------------------------------------------
    def _in_diameter_mode(self):
        # stat.gcodes holds G7 as 70 and G8 as 80 in the lathe-diameter group.
        return 70 in STATUS.stat.gcodes

    def _diameter_mode(self, diameter):
        self.x_label.setText("X Ø" if diameter else "X r")

    def _log(self, text):
        LOG.info(text)
        self.messages.appendPlainText(text)


def get_handlers(halcomp, widgets, paths):
    return [HandlerClass(halcomp, widgets, paths)]
