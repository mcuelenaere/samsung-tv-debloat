using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;

namespace TvAuthUi
{
    internal static class Program
    {
        private const string LogPath = "/tmp/tvauth-ui.log";
        private static readonly object LogLock = new object();
        private static readonly ConcurrentQueue<Action> UiQueue = new ConcurrentQueue<Action>();

        private static readonly IntPtr AppId = Interop.Ansi("org.tvauth.ui");
        private static readonly IntPtr DeleteRequest = Interop.Ansi("delete,request");
        private static readonly IntPtr Clicked = Interop.Ansi("clicked");
        private static readonly IntPtr BackKey = Interop.Ansi("XF86Back");

        private static readonly Interop.AppCreateCb AppCreateDelegate = AppCreate;
        private static readonly Interop.AppLifecycleCb AppTerminateDelegate = AppTerminate;
        private static readonly Interop.AppLifecycleCb AppPauseDelegate = AppPause;
        private static readonly Interop.AppLifecycleCb AppResumeDelegate = AppResume;
        private static readonly Interop.AppControlCb AppControlDelegate = AppControl;
        private static readonly Interop.EvasSmartCb BackDelegate = OnBack;
        private static readonly Interop.EvasSmartCb DecisionDelegate = OnDecisionClicked;
        private static readonly Interop.EvasSmartCb RevokeDelegate = OnRevokeClicked;
        private static readonly Interop.EvasSmartCb NotifyDelegate = OnNotifyClicked;
        private static readonly Interop.EvasSmartCb RefreshDelegate = OnRefreshClicked;
        private static readonly Interop.EcoreTimerCb TimerDelegate = OnTimer;

        // Palette (RGB). Rectangles are opaque except the scrim.
        private const int ColorCard = 0x1c1f26;
        private const int ColorInset = 0x0e1014;
        private const int ColorRow = 0x272b34;
        private const int ColorDanger = 0xe5484d;
        private const int ColorAccent = 0x3e7bfa;
        private const int CardWidth = 1200;
        private const int CardPadding = 56;
        // Up to this many privileges are listed; the rest are summarised.
        private const int MaxPrivilegesShown = 7;

        /// <summary>Friendly labels for the common public Tizen privileges.</summary>
        private static readonly Dictionary<string, string> PrivilegeNames =
            new Dictionary<string, string>
            {
                { "http://tizen.org/privilege/internet", "Access the internet" },
                { "http://tizen.org/privilege/filesystem.read", "Read shared files" },
                { "http://tizen.org/privilege/filesystem.write", "Write shared files" },
                { "http://tizen.org/privilege/camera", "Use the camera" },
                { "http://tizen.org/privilege/microphone", "Use the microphone" },
                { "http://tizen.org/privilege/location", "Use the TV's location" },
                { "http://tizen.org/privilege/contacts.read", "Read contacts" },
                { "http://tizen.org/privilege/contacts.write", "Write contacts" },
                { "http://tizen.org/privilege/calendar.read", "Read the calendar" },
                { "http://tizen.org/privilege/calendar.write", "Write the calendar" },
                { "http://tizen.org/privilege/message.read", "Read messages" },
                { "http://tizen.org/privilege/message.write", "Write messages" },
                { "http://tizen.org/privilege/bluetooth", "Use Bluetooth" },
                { "http://tizen.org/privilege/nfc", "Use NFC" },
                { "http://tizen.org/privilege/download", "Download files" },
                { "http://tizen.org/privilege/appmanager.launch", "Launch other apps" },
                { "http://tizen.org/privilege/network.get", "Read network information" },
                { "http://tizen.org/privilege/network.set", "Change network settings" },
                { "http://tizen.org/privilege/systeminfo.get", "Read system information" },
                { "http://tizen.org/privilege/systeminfo.set", "Change system information" },
                { "http://tizen.org/privilege/telephony", "Use telephony" },
                { "http://tizen.org/privilege/healthinfo", "Read health information" },
                { "http://tizen.org/privilege/recorder", "Record audio" },
                { "http://tizen.org/privilege/vibration", "Control vibration" },
                { "http://tizen.org/privilege/alarm", "Set alarms" },
                { "http://tizen.org/privilege/power", "Control power" },
                { "http://tizen.org/privilege/usb.host", "Use USB devices" },
                { "http://tizen.org/privilege/mediacontroller", "Control media playback" },
                { "http://tizen.org/privilege/d2d.datasharing", "Share data with nearby devices" }
            };

        // Execution-notice toast (top-right). Tune these on the TV.
        private static readonly IntPtr ToastName = Interop.Ansi("org.tvauth.ui.toast");
        private const int ToastWidth = 640;
        private const int ToastPadding = 28;
        private const int ToastMargin = 48;
        private const int ToastShownSeconds = 6;
        private const int ToastMaxApps = 3;
        private const int ToastCommandChars = 52;
        // Stay connected (invisible) this long after a toast so the next notice
        // shows instantly instead of paying ~1 s for an app launch. Must stay
        // below the daemon's 600 s idle timeout for the UI connection.
        private const int IdleLingerSeconds = 300;

        private static IntPtr window;
        private static IntPtr accentBar;
        private static IntPtr countdownLabel;
        private static int lastCountdown = -1;
        private static IntPtr headerLabel;
        private static IntPtr statusLabel;
        private static IntPtr contentBox;
        private static readonly List<IntPtr> contentObjects = new List<IntPtr>();
        private static readonly List<IntPtr> revokeHandles = new List<IntPtr>();

        private static BrokerClient broker;
        private static PromptInfo currentPrompt;
        private static List<GrantInfo> grants = new List<GrantInfo>();
        private static string managerMessage;
        private static DateTime? promptExpiresAt;
        private static DateTime? exitAt;
        private static bool exiting;
        private static bool backGrabbed;
        private static bool connected;
        private static int screenW = 1920;
        private static int screenH = 1080;
        private static bool mainShown;
        private static bool helloSeen;
        private static int appControlCount;
        private static bool exitWhenIdle;
        private static DateTime? lingerUntil;

        private static IntPtr toastWindow;
        private static IntPtr toastLabel;
        private static DateTime? toastHideAt;
        private static int toastRows;
        private static string focusNotifyLabel;
        private static readonly List<NoticeInfo> toastEntries = new List<NoticeInfo>();

        private static int Main(string[] args)
        {
            var callbacks = new Interop.UiAppLifecycle
            {
                create = Marshal.GetFunctionPointerForDelegate(AppCreateDelegate),
                terminate = Marshal.GetFunctionPointerForDelegate(AppTerminateDelegate),
                pause = Marshal.GetFunctionPointerForDelegate(AppPauseDelegate),
                resume = Marshal.GetFunctionPointerForDelegate(AppResumeDelegate),
                appControl = Marshal.GetFunctionPointerForDelegate(AppControlDelegate)
            };

            var argv = AllocateArgv(args);
            try
            {
                return Interop.ui_app_main(args.Length, argv, ref callbacks, IntPtr.Zero);
            }
            finally
            {
                FreeArgv(argv, args.Length);
            }
        }

        private static bool AppCreate(IntPtr userData)
        {
            Log("app_create");
            window = Interop.elm_win_add(IntPtr.Zero, AppId, Interop.ElmWinNotification);
            if (window == IntPtr.Zero)
            {
                Log("elm_win_add failed");
                return false;
            }

            var waylandWindow = Interop.elm_win_wl_window_get(window);
            if (waylandWindow != IntPtr.Zero)
            {
                Interop.ecore_wl2_window_type_set(
                    waylandWindow,
                    Interop.EcoreWl2WindowTypeNotification);
                Log("set Wayland notification type");
            }

            SetWindowTitle("Root access");
            Interop.elm_win_modal_set(window, 1);
            Interop.elm_win_floating_mode_set(window, 1);
            SetHighestNotificationLevel(window);

            Interop.evas_object_smart_callback_add(window, DeleteRequest, BackDelegate, IntPtr.Zero);
            Interop.eext_object_event_callback_add(
                window,
                Interop.EextCallbackBack,
                BackDelegate,
                IntPtr.Zero);

            // Full-screen dimmed scrim with a solid, centered card. The old
            // elm_bg is transparent in the TV theme, so the home screen showed
            // straight through the prompt. If the compositor ignores alpha the
            // scrim simply renders opaque black, which is also fine.
            Interop.elm_win_alpha_set(window, 1);
            var scrim = CreateRect(window, 0x000000, 140);
            Interop.evas_object_size_hint_weight_set(scrim, 1.0, 1.0);
            Interop.elm_win_resize_object_add(window, scrim);

            var layout = Interop.elm_table_add(window);
            if (layout == IntPtr.Zero)
                return false;
            Interop.evas_object_size_hint_weight_set(layout, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(layout, -1.0, -1.0);
            Interop.elm_win_resize_object_add(window, layout);
            Interop.evas_object_show(layout);

            var card = Panel(layout, ColorCard, CardPadding, CardPadding, out var cardBox);
            Interop.evas_object_size_hint_weight_set(card, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(card, 0.5, 0.5);
            Interop.elm_table_pack(layout, card, 0, 0, 1, 1);
            Interop.elm_box_padding_set(cardBox, 0, 18);

            accentBar = CreateRect(card, ColorAccent, 255);
            Interop.evas_object_size_hint_min_set(accentBar, CardWidth, 8);
            Interop.evas_object_size_hint_weight_set(accentBar, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(accentBar, -1.0, 0.0);
            Interop.elm_table_pack(card, accentBar, 0, 0, 1, 1);

            headerLabel = AddToBox(cardBox, CreateLabel(cardBox, ""));
            statusLabel = AddToBox(cardBox, CreateLabel(cardBox, ""));
            SetHeader("TV-AUTHD", "Root access manager", ColorAccent);
            SetStatus("Connecting to tv-authd...");

            contentBox = Interop.elm_box_add(cardBox);
            Interop.evas_object_size_hint_weight_set(contentBox, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(contentBox, -1.0, -1.0);
            Interop.elm_box_padding_set(contentBox, 0, 24);
            Interop.elm_box_pack_end(cardBox, contentBox);
            Interop.evas_object_show(contentBox);

            RenderManager();

            Interop.ecore_timer_add(0.1, TimerDelegate, IntPtr.Zero);

            broker = new BrokerClient(UiQueue, Log);
            broker.PromptReceived += OnPrompt;
            broker.NoticeReceived += OnNotice;
            broker.HelloAccepted += OnHelloAccepted;
            broker.GrantsReceived += OnGrants;
            broker.DecisionFinished += OnDecisionFinished;
            broker.RevokeFinished += OnRevokeFinished;
            broker.NotifyFinished += OnNotifyFinished;
            broker.ConnectionChanged += OnConnectionChanged;
            broker.StatusMessage += OnStatusMessage;
            broker.Start();

            Interop.elm_win_screen_size_get(window, out _, out _, out var w, out var h);
            if (w > 0 && h > 0)
            {
                screenW = w;
                screenH = h;
            }
            Log("screen " + screenW + "x" + screenH);
            Interop.evas_object_resize(window, screenW, screenH);
            Interop.elm_win_center(window, 1, 1);
            // The full-screen window stays hidden until we know why we were
            // launched: a notice-only launch must not cover the screen.
            return true;
        }

        private static void ShowMain()
        {
            HideToast();
            exitWhenIdle = false;
            lingerUntil = null;
            if (mainShown || window == IntPtr.Zero)
                return;
            mainShown = true;
            if (currentPrompt == null)
            {
                // A prompt may have granted or revoked an app since the last list;
                // never render the manager from a stale cache.
                Log("manager grants refresh");
                broker?.RequestGrants();
            }
            backGrabbed = Interop.eext_win_keygrab_set(window, BackKey) == 1;
            if (!backGrabbed)
                Log("XF86Back keygrab failed");
            Interop.evas_object_show(window);
            Interop.elm_win_raise(window);
            Interop.elm_win_activate(window);
            Log("window shown/raised/activated");
        }

        private static void HideToast()
        {
            toastHideAt = null;
            toastEntries.Clear();
            toastRows = 0;
            if (toastWindow != IntPtr.Zero)
                Interop.evas_object_hide(toastWindow);
        }

        private static void HideMain()
        {
            if (!mainShown)
                return;
            mainShown = false;
            if (backGrabbed)
                Interop.eext_win_keygrab_unset(window, BackKey);
            backGrabbed = false;
            Interop.evas_object_hide(window);
            Log("window hidden");
        }

        private static void AppTerminate(IntPtr userData)
        {
            broker?.Stop();
            broker = null;
            if (window == IntPtr.Zero)
                return;
            if (backGrabbed)
                Interop.eext_win_keygrab_unset(window, BackKey);
            Interop.evas_object_del(window);
            window = IntPtr.Zero;
            if (toastWindow != IntPtr.Zero)
                Interop.evas_object_del(toastWindow);
            toastWindow = IntPtr.Zero;
        }

        private static void AppPause(IntPtr userData)
        {
        }

        private static void AppResume(IntPtr userData)
        {
        }

        private static void AppControl(IntPtr appControl, IntPtr userData)
        {
            // The first call is our own launch. A later one means the icon was
            // opened while we were running invisibly for a toast.
            if (appControlCount++ == 0)
                return;
            Log("relaunched while running");
            if (currentPrompt == null)
            {
                exitAt = null;
                managerMessage = null;
                if (mainShown)
                {
                    Log("manager grants refresh");
                    broker?.RequestGrants();
                }
                RenderManager();
            }
            ShowMain();
        }

        private static bool OnTimer(IntPtr data)
        {
            while (UiQueue.TryDequeue(out var action))
            {
                try
                {
                    action();
                }
                catch (Exception error)
                {
                    Log("dispatch: " + error);
                }
            }

            if (currentPrompt != null && promptExpiresAt.HasValue && countdownLabel != IntPtr.Zero)
            {
                var remaining = (int)Math.Ceiling((promptExpiresAt.Value - DateTime.UtcNow).TotalSeconds);
                if (remaining != lastCountdown && remaining >= 0)
                {
                    lastCountdown = remaining;
                    SetText(countdownLabel, Small("Denied automatically in " + remaining + " s", "#8a8f99"));
                }
            }

            if (currentPrompt != null
                && promptExpiresAt.HasValue
                && DateTime.UtcNow >= promptExpiresAt.Value)
            {
                Log("prompt expired");
                currentPrompt = null;
                promptExpiresAt = null;
                managerMessage = "The request timed out and was denied.";
                RenderManager();
                exitAt = DateTime.UtcNow.AddSeconds(2);
            }

            if (exitAt.HasValue && DateTime.UtcNow >= exitAt.Value)
            {
                exitAt = null;
                HideMain();
                lingerUntil = null;
                exitWhenIdle = true;
            }

            if (toastHideAt.HasValue && DateTime.UtcNow >= toastHideAt.Value)
            {
                HideToast();
                if (!mainShown)
                {
                    exitWhenIdle = true;
                    lingerUntil = DateTime.UtcNow.AddSeconds(IdleLingerSeconds);
                }
            }

            if (exitWhenIdle
                && !mainShown
                && !toastHideAt.HasValue
                && (!lingerUntil.HasValue || DateTime.UtcNow >= lingerUntil.Value))
            {
                ExitNow();
                return false;
            }
            return true;
        }

        private static void OnBack(IntPtr data, IntPtr obj, IntPtr eventInfo)
        {
            Log("back key");
            if (currentPrompt != null)
            {
                SendDecision("deny");
                return;
            }
            ExitNow();
        }

        private static void OnDecisionClicked(IntPtr data, IntPtr obj, IntPtr eventInfo)
        {
            var decision = (int)data.ToInt64();
            Log("decision clicked " + decision);
            switch (decision)
            {
                case 2:
                    SendDecision("once");
                    break;
                case 3:
                    SendDecision("always");
                    break;
                default:
                    SendDecision("deny");
                    break;
            }
        }

        private static void OnRevokeClicked(IntPtr data, IntPtr obj, IntPtr eventInfo)
        {
            if (data == IntPtr.Zero)
                return;
            var target = GCHandle.FromIntPtr(data).Target as string;
            if (string.IsNullOrEmpty(target))
                return;
            Log("revoke clicked " + target);
            managerMessage = "Revoking " + target + "...";
            RenderManager();
            broker?.Revoke(target);
        }

        private static void OnRefreshClicked(IntPtr data, IntPtr obj, IntPtr eventInfo)
        {
            Log("refresh clicked");
            managerMessage = "Refreshing...";
            RenderManager();
            broker?.RequestGrants();
        }

        private static void OnPrompt(PromptInfo prompt)
        {
            if (currentPrompt != null)
            {
                // The daemon normally keeps only one prompt active. If a stale
                // prompt is replaced, deny it rather than silently dropping it.
                broker?.Decide(currentPrompt, "deny");
            }
            currentPrompt = prompt;
            exitAt = null;
            ShowMain();
            promptExpiresAt = DateTime.UtcNow.AddMilliseconds(
                prompt.TimeoutMs > 0 ? prompt.TimeoutMs : 60_000);
            Log("prompt received package=" + prompt.Package + " request=" + prompt.RequestId
                + " kind=" + prompt.Kind);
            if (prompt.IsPackageOperation)
                RenderPackagePrompt();
            else
                RenderPrompt();
        }

        private static void OnHelloAccepted(string launch)
        {
            // Only the first registration decides; a reconnect after a broker
            // restart must not pop the manager over whatever the user watches.
            if (!helloSeen)
            {
                helloSeen = true;
                if (launch == "notice" && !mainShown && appControlCount <= 1)
                {
                    Log("notice-only launch");
                    exitWhenIdle = true;
                    lingerUntil = DateTime.UtcNow.AddSeconds(IdleLingerSeconds);
                    return;
                }
                ShowMain();
                return;
            }

            if (mainShown && currentPrompt == null)
            {
                Log("manager grants refresh");
                broker?.RequestGrants();
            }
        }

        private static void OnNotice(List<NoticeInfo> notices)
        {
            foreach (var notice in notices)
            {
                Log("notice package=" + notice.Package + " count=" + notice.Count);
                if (mainShown)
                    continue;
                var entry = toastEntries.FirstOrDefault(
                    e => e.Label == notice.Label && e.Kind == notice.Kind);
                if (entry == null)
                {
                    toastEntries.Add(notice);
                }
                else
                {
                    entry.Count += notice.Count;
                    entry.Command = notice.Command;
                    entry.CommandTruncated = notice.CommandTruncated;
                }
            }
            if (mainShown || toastEntries.Count == 0)
                return;
            ShowToast();
        }

        private static void ShowToast()
        {
            if (toastWindow == IntPtr.Zero && !CreateToastWindow())
                return;
            SetText(toastLabel, ToastMarkup());
            // Live updates only change the text; re-layout the window when it
            // appears or gains a row, so counters tick without flicker.
            var rows = Math.Min(toastEntries.Count, ToastMaxApps + 1);
            if (!toastHideAt.HasValue || rows != toastRows)
            {
                toastRows = rows;
                // Elementary grows the window to the content's minimum height.
                Interop.evas_object_resize(toastWindow, ToastWidth + 2 * ToastPadding, 1);
                Interop.evas_object_move(toastWindow,
                    screenW - ToastWidth - 2 * ToastPadding - ToastMargin, ToastMargin);
                Interop.evas_object_show(toastWindow);
            }
            toastHideAt = DateTime.UtcNow.AddSeconds(ToastShownSeconds);
        }

        private static bool CreateToastWindow()
        {
            toastWindow = Interop.elm_win_add(IntPtr.Zero, ToastName, Interop.ElmWinNotification);
            if (toastWindow == IntPtr.Zero)
            {
                Log("toast elm_win_add failed");
                return false;
            }
            var waylandWindow = Interop.elm_win_wl_window_get(toastWindow);
            if (waylandWindow != IntPtr.Zero)
                Interop.ecore_wl2_window_type_set(waylandWindow, Interop.EcoreWl2WindowTypeNotification);
            // Never take focus or keys from the app the user is watching.
            Interop.elm_win_prop_focus_skip_set(toastWindow, 1);
            SetHighestNotificationLevel(toastWindow);
            var hint = Interop.Ansi("wm.policy.win.user.geometry");
            var one = Interop.Ansi("1");
            try { Interop.elm_win_aux_hint_add(toastWindow, hint, one); }
            finally
            {
                Marshal.FreeHGlobal(hint);
                Marshal.FreeHGlobal(one);
            }

            var card = Panel(toastWindow, ColorCard, ToastPadding, ToastPadding - 6, out var box);
            Interop.elm_win_resize_object_add(toastWindow, card);
            var bar = CreateRect(box, ColorDanger, 255);
            Interop.evas_object_size_hint_min_set(bar, ToastWidth, 6);
            AddToBox(box, bar);
            toastLabel = AddToBox(box, CreateLabel(box, ""));
            Interop.elm_label_line_wrap_set(toastLabel, 0);
            return true;
        }

        private static string ToastMarkup()
        {
            var packageChange = toastEntries.Any(e => e.Kind != "exec");
            var builder = new StringBuilder(packageChange
                ? "<font_size=18><b><color=#e5484d>PACKAGE CHANGE</color></b></font_size>"
                : "<font_size=18><b><color=#e5484d>ROOT COMMAND</color></b></font_size>");
            foreach (var entry in toastEntries.Take(ToastMaxApps))
            {
                string what;
                if (entry.Kind == "install")
                {
                    what = entry.Count == 1
                        ? " installed a package"
                        : " installed " + entry.Count + " packages";
                }
                else if (entry.Kind == "uninstall")
                {
                    what = entry.Count == 1
                        ? " removed a package"
                        : " removed " + entry.Count + " packages";
                }
                else
                {
                    what = entry.Count == 1
                        ? " ran a command as root"
                        : " ran " + entry.Count + " commands as root";
                }
                builder.Append("<br/><font_size=26><b>")
                    .Append(Escape(Shorten(entry.Package, 36)))
                    .Append("</b>").Append(what).Append("</font_size><br/>")
                    .Append(Small("$ " + Shorten(entry.Command, ToastCommandChars, entry.CommandTruncated),
                        "#8a8f99"));
            }
            if (toastEntries.Count > ToastMaxApps)
            {
                var more = toastEntries.Count - ToastMaxApps;
                builder.Append("<br/>").Append(Small(
                    "+ " + more + (more == 1 ? " more app" : " more apps"), "#8a8f99"));
            }
            return builder.ToString();
        }

        /// <summary>First line only, cut to fit the toast; marks any cut with an ellipsis.</summary>
        private static string Shorten(string text, int max, bool alreadyTruncated = false)
        {
            text = text ?? "";
            var newline = text.IndexOf('\n');
            var cut = alreadyTruncated || newline >= 0;
            if (newline >= 0)
                text = text.Substring(0, newline).TrimEnd('\r');
            if (text.Length > max)
            {
                text = text.Substring(0, SafePrefixLength(text, max - 1));
                cut = true;
            }
            return cut ? text + "\u2026" : text;
        }

        private static int SafePrefixLength(string text, int maxChars)
        {
            if (string.IsNullOrEmpty(text) || maxChars >= text.Length)
                return text?.Length ?? 0;
            if (maxChars <= 0)
                return 0;
            return char.IsHighSurrogate(text[maxChars - 1]) ? maxChars - 1 : maxChars;
        }

        private static void OnGrants(List<GrantInfo> list)
        {
            grants = list ?? new List<GrantInfo>();
            if (managerMessage == "Refreshing...")
                managerMessage = null;
            if (currentPrompt == null && !exitAt.HasValue)
                RenderManager();
        }

        private static void OnDecisionFinished(bool ok, string error)
        {
            if (currentPrompt != null)
                return;
            if (ok)
            {
                SetStatus("Closing...");
                exitAt = DateTime.UtcNow.AddSeconds(1.2);
            }
            else
            {
                var detail = error switch
                {
                    "package_identity_unavailable" =>
                        "the installed package identity could not be verified; choose Allow once or Deny",
                    "grant_persist_failed" =>
                        "the grant could not be saved; choose Allow once or Deny",
                    _ => string.IsNullOrEmpty(error) ? "unknown" : error
                };
                SetHeader("ERROR", "Decision failed", ColorDanger);
                SetStatus("tv-authd reported: " + detail + ". The request was not approved.");
                exitAt = DateTime.UtcNow.AddSeconds(2.5);
            }
        }

        // Cycle order of the per-grant toast setting.
        private static string NextNotify(string notify) =>
            notify == "every" ? "first" : notify == "first" ? "never" : "every";

        private static string NotifyButtonText(string notify) =>
            notify == "first" ? "Toast: first use" : notify == "never" ? "Toast: never" : "Toast: every";

        private static string NotifyDescription(string notify) =>
            notify == "first"
                ? "Toast on first command after 30 min idle"
                : notify == "never" ? "No toasts" : "Toast on every command";

        private static void OnNotifyClicked(IntPtr data, IntPtr obj, IntPtr eventInfo)
        {
            if (data == IntPtr.Zero)
                return;
            var label = GCHandle.FromIntPtr(data).Target as string;
            var grant = grants.FirstOrDefault(g => g.Label == label);
            if (grant == null)
                return;
            var next = NextNotify(grant.Notify);
            Log("notify clicked " + label + " -> " + next);
            focusNotifyLabel = label;
            broker?.SetNotify(label, next);
        }

        private static void OnNotifyFinished(bool ok, string error)
        {
            if (currentPrompt != null || ok)
                return;
            managerMessage = "Could not change toast setting: "
                + (string.IsNullOrEmpty(error) ? "unknown" : error);
            RenderManager();
        }

        private static void OnRevokeFinished(bool revoked, string error)
        {
            if (currentPrompt != null)
                return;
            if (revoked)
            {
                managerMessage = "Grant revoked.";
                RenderManager();
            }
            else
            {
                managerMessage = error == "not_found"
                    ? "That grant was already removed."
                    : "Revoke failed: " + (string.IsNullOrEmpty(error) ? "unknown" : error);
                RenderManager();
            }
        }

        private static void OnConnectionChanged(bool isConnected)
        {
            connected = isConnected;
            // Could not even register: show the manager with the error.
            if (!isConnected && !helloSeen)
                ShowMain();
            if (!isConnected)
            {
                if (currentPrompt != null)
                {
                    currentPrompt = null;
                    promptExpiresAt = null;
                    managerMessage = "Connection lost. The pending request was cancelled.";
                }
                else if (string.IsNullOrEmpty(managerMessage))
                {
                    managerMessage = "Connection lost. Retrying...";
                }
            }
            else if (managerMessage == "Connection lost. Retrying...")
            {
                managerMessage = null;
            }

            if (currentPrompt == null && !exitAt.HasValue)
                RenderManager();
        }

        private static void OnStatusMessage(string message)
        {
            if (currentPrompt == null && !exitAt.HasValue)
            {
                managerMessage = message;
                RenderManager();
            }
        }

        private static void SendDecision(string decision)
        {
            var prompt = currentPrompt;
            if (prompt == null || broker == null)
                return;
            currentPrompt = null;
            promptExpiresAt = null;
            exitAt = DateTime.UtcNow.AddSeconds(5);
            ClearContent();
            if (prompt.IsPackageOperation)
            {
                var removing = prompt.Kind == "uninstall";
                if (decision == "deny")
                {
                    SetHeader(removing ? "REMOVE PACKAGE" : "INSTALL PACKAGE", "Denied", ColorDanger);
                    SetStatus("The package was not changed.");
                }
                else
                {
                    SetHeader(removing ? "REMOVE PACKAGE" : "INSTALL PACKAGE", "Approved",
                        ColorAccent);
                    SetStatus(removing
                        ? "tv-authd is removing the package..."
                        : "tv-authd is installing the package...");
                }
            }
            else
            {
                switch (decision)
                {
                    case "once":
                        SetHeader("ROOT ACCESS", "Allowed once", ColorAccent);
                        break;
                    case "always":
                        SetHeader("ROOT ACCESS", "Always allowed", ColorAccent);
                        break;
                    default:
                        SetHeader("ROOT ACCESS", "Denied", ColorDanger);
                        break;
                }
                SetStatus("Sending decision...");
            }
            broker.Decide(prompt, decision);
            Log("decision sent package=" + prompt.Package + " kind=" + prompt.Kind
                + " decision=" + decision);
        }

        private static void RenderPrompt()
        {
            if (currentPrompt == null)
                return;
            var prompt = currentPrompt;
            ClearContent();
            SetHeader("ROOT ACCESS REQUEST", "An app wants full root access", ColorDanger);
            SetStatus("Root can read, change or erase anything on this TV, including other apps' data "
                + "and system files. Only allow apps you trust.");

            var app = Panel(contentBox, ColorRow, 32, 24, out var appBox);
            AddContent(app);
            AddToBox(appBox, CreateLabel(appBox, Small("APPLICATION", "#8a8f99")));
            AddToBox(appBox, CreateLabel(appBox,
                "<font_size=34><b>" + Escape(prompt.Package) + "</b></font_size>"));
            AddToBox(appBox, CreateLabel(appBox, Small(prompt.Label, "#8a8f99")));

            AddContent(CreateLabel(contentBox, Small("COMMAND", "#8a8f99")));
            var command = Panel(contentBox, ColorInset, 28, 20, out var commandBox);
            AddContent(command);
            AddToBox(commandBox, CreateLabel(commandBox,
                "<font=Monospace><font_size=22><color=#e8e8e8>"
                + CommandMarkup(prompt.Command, prompt.CommandTruncated)
                + "</color></font_size></font>"));

            AddContent(CreateLabel(contentBox, Small(
                "<b>Allow once</b> runs only this command. <b>Always allow</b> lets this app run any "
                + "command as root without asking again; revoke it later in Root access manager.",
                "#b4b9c2", escape: false)));

            var footer = CreateRow(contentBox);
            AddContent(footer);
            lastCountdown = -1;
            countdownLabel = AddToBox(footer, CreateLabel(footer, ""));
            var deny = CreateButton(footer, "Deny", DecisionDelegate, (IntPtr)1);
            CreateButton(footer, "Allow once", DecisionDelegate, (IntPtr)2);
            CreateButton(footer, "Always allow", DecisionDelegate, (IntPtr)3);
            Interop.elm_object_focus_set(deny, 1);
        }

        /// <summary>
        /// Package prompts show what is actually being installed instead of the
        /// root-access warning. There is no "always" option: every package
        /// operation is a fresh decision (protocol §5.4).
        /// </summary>
        private static void RenderPackagePrompt()
        {
            var prompt = currentPrompt;
            if (prompt == null)
                return;
            var removing = prompt.Kind == "uninstall";
            var info = prompt.Install;
            var target = prompt.Uninstall;
            ClearContent();
            if ((!removing && info == null) || (removing && target == null))
            {
                // Fail closed: never offer an approval button for a package
                // request the daemon could not describe.
                SetHeader(removing ? "REMOVE PACKAGE" : "INSTALL PACKAGE",
                    "Malformed package request", ColorDanger);
                SetStatus("tv-authd did not describe the package. This request cannot be "
                    + "approved.");
                var rejectRow = CreateRow(contentBox);
                AddContent(rejectRow);
                var reject = CreateButton(rejectRow, "Deny", DecisionDelegate, (IntPtr)1);
                Interop.elm_object_focus_set(reject, 1);
                return;
            }

            SetHeader(
                removing ? "REMOVE PACKAGE" : "INSTALL PACKAGE",
                removing ? "An app wants to remove a package" : "An app wants to install a package",
                removing ? ColorDanger : ColorAccent);
            if (removing)
            {
                SetStatus("The package and its data will be removed from this TV.");
            }
            else if (info.IsUpgrade)
            {
                SetStatus("This replaces the installed version. Check the publisher "
                    + "before continuing.");
            }
            else
            {
                SetStatus("Only install packages you trust. Apps can change how this TV "
                    + "behaves and reach your data.");
            }

            var app = Panel(contentBox, ColorRow, 32, 24, out var appBox);
            AddContent(app);
            AddToBox(appBox, CreateLabel(appBox, Small("REQUESTED BY", "#8a8f99")));
            AddToBox(appBox, CreateLabel(appBox,
                "<font_size=34><b>" + Escape(prompt.Package) + "</b></font_size>"));
            AddToBox(appBox, CreateLabel(appBox, Small(prompt.Label, "#8a8f99")));

            var name = removing
                ? (!string.IsNullOrEmpty(target.Name) ? target.Name : target.PkgId)
                : (info != null && !string.IsNullOrEmpty(info.Name) ? info.Name : prompt.Package);
            var details = new List<string>();
            if (removing)
            {
                if (!string.IsNullOrEmpty(target.PkgId))
                    details.Add(target.PkgId);
                if (!string.IsNullOrEmpty(target.PkgVersion))
                    details.Add("version " + target.PkgVersion);
                if (!string.IsNullOrEmpty(target.PkgType))
                    details.Add(target.PkgType.ToUpperInvariant());
            }
            else if (info != null)
            {
                if (!string.IsNullOrEmpty(info.PkgId))
                    details.Add(info.PkgId);
                if (!string.IsNullOrEmpty(info.PkgVersion))
                    details.Add("version " + info.PkgVersion);
                if (!string.IsNullOrEmpty(info.PkgType))
                    details.Add(info.PkgType.ToUpperInvariant());
                if (info.SizeBytes > 0)
                    details.Add(FormatSize(info.SizeBytes));
            }

            var packagePanel = Panel(contentBox, ColorInset, 28, 20, out var packageBox);
            AddContent(packagePanel);
            AddToBox(packageBox, CreateLabel(packageBox,
                Small(removing ? "PACKAGE" : "PACKAGE TO INSTALL", "#8a8f99")));
            AddToBox(packageBox, CreateLabel(packageBox,
                "<font_size=32><b>" + Escape(name) + "</b></font_size>"));
            if (details.Count > 0)
            {
                AddToBox(packageBox, CreateLabel(packageBox,
                    Small(string.Join(" \u00b7 ", details), "#b4b9c2")));
            }
            if (info != null && info.IsUpgrade)
            {
                AddToBox(packageBox, CreateLabel(packageBox,
                    Small("This upgrades the installed version.", "#ffb74d")));
            }
            if (!removing)
            {
                AddToBox(packageBox, CreateLabel(packageBox,
                    SignatureBadge(info?.Signature)));
            }

            if (!removing && info != null && info.Privileges.Count > 0)
            {
                AddContent(CreateLabel(contentBox,
                    Small("PRIVILEGES (" + info.Privileges.Count + ")", "#8a8f99")));
                var privileges = Panel(contentBox, ColorRow, 28, 20, out var privilegeBox);
                AddContent(privileges);
                var shown = 0;
                foreach (var privilege in info.Privileges)
                {
                    if (shown++ >= MaxPrivilegesShown)
                        break;
                    AddToBox(privilegeBox, CreateLabel(privilegeBox,
                        Small("\u2022 " + PrivilegeLabel(privilege), "#e8e8e8")));
                }
                if (info.Privileges.Count > MaxPrivilegesShown)
                {
                    var more = info.Privileges.Count - MaxPrivilegesShown;
                    AddToBox(privilegeBox, CreateLabel(privilegeBox,
                        Small("+ " + more + (more == 1 ? " more privilege" : " more privileges"),
                            "#8a8f99")));
                }
            }

            var footer = CreateRow(contentBox);
            AddContent(footer);
            lastCountdown = -1;
            countdownLabel = AddToBox(footer, CreateLabel(footer, ""));
            var deny = CreateButton(footer, "Deny", DecisionDelegate, (IntPtr)1);
            CreateButton(footer, removing ? "Remove" : "Install", DecisionDelegate, (IntPtr)2);
            Interop.elm_object_focus_set(deny, 1);
        }

        private static string SignatureBadge(string signature)
        {
            switch (signature)
            {
                case "unsigned_will_resign":
                    return Small("Unsigned \u2014 it will be signed with this TV's CA.", "#ffb74d");
                case "foreign_will_resign":
                    return Small("Third-party signature \u2014 it will be re-signed with this TV's CA.",
                        "#ffb74d");
                case "user_ca":
                    return Small("Signed by your own CA.", "#66bb6a");
                case "samsung":
                    return Small("Signed by Samsung.", "#90a4ae");
                default:
                    return Small("Signed package \u2014 publisher not identified.", "#ffb74d");
            }
        }

        private static string PrivilegeLabel(string uri)
        {
            if (string.IsNullOrEmpty(uri))
                return "";
            if (PrivilegeNames.TryGetValue(uri, out var friendly))
                return friendly;
            const string prefix = "http://tizen.org/privilege/";
            if (uri.StartsWith(prefix, StringComparison.Ordinal))
                return uri.Substring(prefix.Length).Replace('.', ' ');
            return uri;
        }

        private static string FormatSize(long bytes)
        {
            const long kilobyte = 1024;
            const long megabyte = kilobyte * 1024;
            const long gigabyte = megabyte * 1024;
            if (bytes >= gigabyte)
                return (bytes / (double)gigabyte).ToString("0.0") + " GB";
            if (bytes >= megabyte)
                return (bytes / (double)megabyte).ToString("0.0") + " MB";
            if (bytes >= kilobyte)
                return (bytes / (double)kilobyte).ToString("0") + " KB";
            return bytes + " B";
        }

        private static void RenderManager()        {
            if (contentBox == IntPtr.Zero)
                return;
            ClearContent();
            SetHeader("TV-AUTHD", "Root access manager", ColorAccent);
            SetStatus(managerMessage ?? "Apps listed here can run commands as root without asking.");

            if (!connected)
            {
                AddContent(CreateLabel(contentBox,
                    Small("Not connected to tv-authd. Retrying automatically.", "#ffb74d")));
            }

            var firstButton = IntPtr.Zero;
            if (grants.Count == 0)
            {
                var empty = Panel(contentBox, ColorRow, 32, 28, out var emptyBox);
                AddContent(empty);
                AddToBox(emptyBox, CreateLabel(emptyBox,
                    "<font_size=28><b>No apps have permanent root access</b></font_size>"));
                AddToBox(emptyBox, CreateLabel(emptyBox, Small(
                    "When an app asks for root and you choose Always allow, it will appear here.",
                    "#8a8f99")));
            }
            else
            {
                foreach (var grant in grants.Take(5))
                {
                    var row = Panel(contentBox, ColorRow, 32, 20, out var rowBox);
                    AddContent(row);
                    Interop.elm_box_horizontal_set(rowBox, 1);
                    Interop.elm_box_padding_set(rowBox, 24, 0);

                    AddToBox(rowBox, CreateLabel(rowBox,
                        "<font_size=28><b>" + Escape(grant.Package) + "</b></font_size><br/>"
                        + Small(grant.Label, "#8a8f99") + "<br/>"
                        + Small(NotifyDescription(grant.Notify), "#b4b9c2")));

                    var handle = GCHandle.Alloc(grant.Label);
                    revokeHandles.Add(GCHandle.ToIntPtr(handle));
                    var notify = CreateButton(rowBox, NotifyButtonText(grant.Notify), NotifyDelegate,
                        GCHandle.ToIntPtr(handle));
                    Interop.evas_object_size_hint_min_set(notify, 320, 84);
                    CreateButton(rowBox, "Revoke", RevokeDelegate, GCHandle.ToIntPtr(handle));
                    // Default focus on the harmless toggle, not Revoke.
                    if (grant.Label == focusNotifyLabel || firstButton == IntPtr.Zero)
                        firstButton = notify;
                }

                if (grants.Count > 5)
                {
                    AddContent(CreateLabel(contentBox,
                        Small("Only the first 5 of " + grants.Count + " apps are shown.", "#8a8f99")));
                }
            }

            var footer = CreateRow(contentBox);
            AddContent(footer);
            AddToBox(footer, CreateLabel(footer, Small("Press BACK to close", "#8a8f99")));
            var refresh = CreateButton(footer, "Refresh", RefreshDelegate, IntPtr.Zero);
            Interop.elm_object_focus_set(firstButton != IntPtr.Zero ? firstButton : refresh, 1);
        }

        private static string Small(string text, string color, bool escape = true)
        {
            return "<font_size=20><color=" + color + ">" + (escape ? Escape(text) : text)
                + "</color></font_size>";
        }

        private static IntPtr CreateRect(IntPtr parent, int rgb, int alpha)
        {
            var rect = Interop.evas_object_rectangle_add(Interop.evas_object_evas_get(parent));
            // Evas colors are premultiplied.
            Interop.evas_object_color_set(rect,
                ((rgb >> 16) & 0xff) * alpha / 255,
                ((rgb >> 8) & 0xff) * alpha / 255,
                (rgb & 0xff) * alpha / 255,
                alpha);
            Interop.evas_object_show(rect);
            return rect;
        }

        /// <summary>A solid rectangle with a padded vertical box stacked on top.</summary>
        private static IntPtr Panel(IntPtr parent, int rgb, int padX, int padY, out IntPtr inner)
        {
            var table = Interop.elm_table_add(parent);
            Interop.evas_object_size_hint_weight_set(table, 1.0, 0.0);
            Interop.evas_object_size_hint_align_set(table, -1.0, 0.5);

            var rect = CreateRect(table, rgb, 255);
            Interop.evas_object_size_hint_weight_set(rect, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(rect, -1.0, -1.0);
            Interop.elm_table_pack(table, rect, 0, 0, 1, 1);

            inner = Interop.elm_box_add(table);
            Interop.evas_object_size_hint_weight_set(inner, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(inner, -1.0, -1.0);
            Interop.evas_object_size_hint_padding_set(inner, padX, padX, padY, padY);
            Interop.elm_box_padding_set(inner, 0, 6);
            Interop.elm_table_pack(table, inner, 0, 0, 1, 1);
            Interop.evas_object_show(inner);

            Interop.evas_object_show(table);
            return table;
        }

        /// <summary>Horizontal row: first label stretches, buttons hug the right edge.</summary>
        private static IntPtr CreateRow(IntPtr parent)
        {
            var row = Interop.elm_box_add(parent);
            Interop.elm_box_horizontal_set(row, 1);
            Interop.elm_box_padding_set(row, 16, 0);
            Interop.evas_object_size_hint_weight_set(row, 1.0, 0.0);
            Interop.evas_object_size_hint_align_set(row, -1.0, 0.5);
            Interop.evas_object_size_hint_padding_set(row, 0, 0, 12, 0);
            Interop.evas_object_show(row);
            return row;
        }

        private static IntPtr CreateLabel(IntPtr parent, string markup)
        {
            var label = Interop.elm_label_add(parent);
            if (label == IntPtr.Zero)
                return IntPtr.Zero;
            SetText(label, markup);
            Interop.elm_label_line_wrap_set(label, Interop.ElmWrapWord);
            Interop.evas_object_size_hint_weight_set(label, 1.0, 0.0);
            Interop.evas_object_size_hint_align_set(label, -1.0, 0.5);
            Interop.evas_object_show(label);
            return label;
        }

        private static IntPtr AddToBox(IntPtr box, IntPtr child)
        {
            if (child == IntPtr.Zero)
                return child;
            Interop.elm_box_pack_end(box, child);
            return child;
        }

        private static void AddContent(IntPtr child)
        {
            if (child == IntPtr.Zero)
                return;
            Interop.elm_box_pack_end(contentBox, child);
            contentObjects.Add(child);
        }

        private static IntPtr CreateButton(
            IntPtr parent,
            string text,
            Interop.EvasSmartCb callback,
            IntPtr data)
        {
            var button = Interop.elm_button_add(parent);
            if (button == IntPtr.Zero)
                return IntPtr.Zero;
            SetText(button, text);
            Interop.evas_object_size_hint_min_set(button, 260, 84);
            Interop.evas_object_size_hint_weight_set(button, 0.0, 0.0);
            Interop.evas_object_size_hint_align_set(button, 0.5, 0.5);
            Interop.evas_object_smart_callback_add(button, Clicked, callback, data);
            Interop.elm_box_pack_end(parent, button);
            Interop.evas_object_show(button);
            return button;
        }

        private static void ClearContent()
        {
            countdownLabel = IntPtr.Zero;
            foreach (var obj in contentObjects)
            {
                if (obj != IntPtr.Zero)
                    Interop.evas_object_del(obj);
            }
            contentObjects.Clear();
            foreach (var handle in revokeHandles)
            {
                try { GCHandle.FromIntPtr(handle).Free(); } catch { }
            }
            revokeHandles.Clear();
        }

        private static void SetHeader(string eyebrow, string title, int accent)
        {
            var hex = "#" + accent.ToString("x6");
            SetText(headerLabel,
                "<font_size=20><b><color=" + hex + ">" + Escape(eyebrow) + "</color></b></font_size><br/>"
                + "<font_size=46><b>" + Escape(title) + "</b></font_size>");
            if (accentBar != IntPtr.Zero)
            {
                Interop.evas_object_color_set(accentBar,
                    (accent >> 16) & 0xff, (accent >> 8) & 0xff, accent & 0xff, 255);
            }
        }

        private static void SetStatus(string text)
        {
            SetText(statusLabel, "<font_size=24><color=#c9ccd3>" + Escape(text) + "</color></font_size>");
        }

        private static void SetWindowTitle(string text)
        {
            var value = Interop.Ansi(text);
            try { Interop.elm_win_title_set(window, value); }
            finally { Marshal.FreeHGlobal(value); }
        }

        private static void SetText(IntPtr obj, string text)
        {
            var value = Interop.Ansi(text);
            try { Interop.elm_object_part_text_set(obj, IntPtr.Zero, value); }
            finally { Marshal.FreeHGlobal(value); }
        }

        private static string CommandMarkup(string command, bool alreadyTruncated)
        {
            if (command == null)
                command = "";
            var lines = new List<string>();
            var truncated = alreadyTruncated;
            foreach (var rawLine in command.Replace("\r\n", "\n").Split('\n'))
            {
                var line = rawLine;
                while (line.Length > 72)
                {
                    var len = SafePrefixLength(line, 72);
                    if (len == 0)
                        len = line.Length;
                    lines.Add(line.Substring(0, len));
                    line = line.Substring(len);
                }
                lines.Add(line);
                if (lines.Count > 12)
                    break;
            }
            if (lines.Count > 12)
            {
                lines = lines.Take(12).ToList();
                truncated = true;
            }
            var builder = new StringBuilder();
            for (var i = 0; i < lines.Count; i++)
            {
                if (i > 0)
                    builder.Append("<br/>");
                builder.Append(Escape(lines[i]));
            }
            if (truncated)
                builder.Append("<br/><color=#8a8f99><i>Command truncated for display.</i></color>");
            return builder.ToString();
        }

        private static string Escape(string value)
        {
            if (string.IsNullOrEmpty(value))
                return "";
            return value
                .Replace("&", "&amp;")
                .Replace("<", "&lt;")
                .Replace(">", "&gt;");
        }

        private static void SetHighestNotificationLevel(IntPtr target)
        {
            var levels = new[]
            {
                Tuple.Create("TOP", Interop.NotificationTop),
                Tuple.Create("HIGH", Interop.NotificationHigh),
                Tuple.Create("MEDIUM", Interop.NotificationMedium),
                Tuple.Create("DEFAULT", Interop.NotificationDefault)
            };
            foreach (var item in levels)
            {
                var result = Interop.efl_util_set_notification_window_level(target, item.Item2);
                if (result == 0)
                {
                    Log("notification level " + item.Item1 + " accepted");
                    return;
                }
                Log("notification level " + item.Item1 + " rejected with " + result);
            }
            Log("using platform-default notification level");
        }

        private static void ExitNow()
        {
            if (exiting)
                return;
            exiting = true;
            Interop.ui_app_exit();
        }

        private static void Log(string message)
        {
            try
            {
                lock (LogLock)
                {
                    var info = new FileInfo(LogPath);
                    if (info.Exists)
                    {
                        if ((info.Attributes & FileAttributes.ReparsePoint) != 0)
                            return;
                        if (info.Length > 64 * 1024)
                            File.Delete(LogPath);
                    }
                    File.AppendAllText(
                        LogPath,
                        DateTime.UtcNow.ToString("O") + " " + message + Environment.NewLine);
                }
            }
            catch
            {
                // Diagnostics must never prevent the consent UI from working.
            }
        }

        private static IntPtr AllocateArgv(string[] args)
        {
            var argv = Marshal.AllocHGlobal(IntPtr.Size * (args.Length + 1));
            for (var i = 0; i < args.Length; i++)
                Marshal.WriteIntPtr(argv, i * IntPtr.Size, Interop.Ansi(args[i]));
            Marshal.WriteIntPtr(argv, args.Length * IntPtr.Size, IntPtr.Zero);
            return argv;
        }

        private static void FreeArgv(IntPtr argv, int count)
        {
            for (var i = 0; i < count; i++)
            {
                var value = Marshal.ReadIntPtr(argv, i * IntPtr.Size);
                if (value != IntPtr.Zero)
                    Marshal.FreeHGlobal(value);
            }
            Marshal.FreeHGlobal(argv);
        }
    }
}
