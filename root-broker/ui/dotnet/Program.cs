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
        private static readonly Interop.EvasSmartCb RefreshDelegate = OnRefreshClicked;
        private static readonly Interop.EcoreTimerCb TimerDelegate = OnTimer;

        private static IntPtr window;
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
            SetHighestNotificationLevel();

            Interop.evas_object_smart_callback_add(window, DeleteRequest, BackDelegate, IntPtr.Zero);
            Interop.eext_object_event_callback_add(
                window,
                Interop.EextCallbackBack,
                BackDelegate,
                IntPtr.Zero);
            backGrabbed = Interop.eext_win_keygrab_set(window, BackKey) == 1;
            if (!backGrabbed)
                Log("XF86Back keygrab failed");

            var background = Interop.elm_bg_add(window);
            if (background == IntPtr.Zero)
                return false;
            Interop.evas_object_size_hint_weight_set(background, 1.0, 1.0);
            Interop.elm_win_resize_object_add(window, background);
            Interop.evas_object_show(background);

            var rootBox = Interop.elm_box_add(window);
            if (rootBox == IntPtr.Zero)
                return false;
            Interop.evas_object_size_hint_weight_set(rootBox, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(rootBox, -1.0, -1.0);
            Interop.elm_box_padding_set(rootBox, 28, 20);
            Interop.elm_win_resize_object_add(window, rootBox);
            Interop.evas_object_show(rootBox);

            headerLabel = CreateLabel(rootBox, "<font_size=42><b>Root access manager</b></font_size>");
            headerLabel = AddToBox(rootBox, headerLabel);
            statusLabel = CreateLabel(rootBox, "<font_size=22>Connecting to tv-authd...</font_size>");
            statusLabel = AddToBox(rootBox, statusLabel);

            contentBox = Interop.elm_box_add(rootBox);
            Interop.evas_object_size_hint_weight_set(contentBox, 1.0, 1.0);
            Interop.evas_object_size_hint_align_set(contentBox, -1.0, -1.0);
            Interop.elm_box_pack_end(rootBox, contentBox);
            Interop.evas_object_show(contentBox);

            RenderManager();

            Interop.ecore_timer_add(0.1, TimerDelegate, IntPtr.Zero);

            broker = new BrokerClient(UiQueue, Log);
            broker.PromptReceived += OnPrompt;
            broker.GrantsReceived += OnGrants;
            broker.DecisionFinished += OnDecisionFinished;
            broker.RevokeFinished += OnRevokeFinished;
            broker.ConnectionChanged += OnConnectionChanged;
            broker.StatusMessage += OnStatusMessage;
            broker.Start();

            Interop.evas_object_resize(window, 1200, 700);
            Interop.elm_win_center(window, 1, 1);
            Interop.evas_object_show(window);
            Interop.elm_win_raise(window);
            Interop.elm_win_activate(window);
            Log("window shown/raised/activated");
            return true;
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
        }

        private static void AppPause(IntPtr userData)
        {
        }

        private static void AppResume(IntPtr userData)
        {
        }

        private static void AppControl(IntPtr appControl, IntPtr userData)
        {
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
                if (!exiting)
                {
                    exiting = true;
                    Interop.ui_app_exit();
                }
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
            promptExpiresAt = DateTime.UtcNow.AddMilliseconds(
                prompt.TimeoutMs > 0 ? prompt.TimeoutMs : 60_000);
            Log("prompt received package=" + prompt.Package + " request=" + prompt.RequestId);
            RenderPrompt();
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
                SetStatus("Decision sent. Closing...");
                exitAt = DateTime.UtcNow.AddSeconds(1.2);
            }
            else
            {
                SetStatus("Decision failed: " + (string.IsNullOrEmpty(error) ? "unknown" : error));
                exitAt = DateTime.UtcNow.AddSeconds(2.5);
            }
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
            SetHeader("Root access requested");
            SetStatus("Sending decision...");
            broker.Decide(prompt, decision);
            Log("decision sent package=" + prompt.Package + " decision=" + decision);
        }

        private static void RenderPrompt()
        {
            if (currentPrompt == null)
                return;
            var prompt = currentPrompt;
            ClearContent();
            SetHeader("Root access requested");
            SetStatus("An application is requesting full root access.");

            AddContent(CreateLabel(contentBox,
                "<font_size=26><b>Package:</b> " + Escape(prompt.Package) + "</font_size>" +
                "<br/><font_size=18>" + Escape(prompt.Label) + "</font_size>"));

            AddContent(CreateLabel(contentBox,
                "<font_size=24><b>This grants FULL ROOT ACCESS.</b></font_size>" +
                "<br/><font_size=20>Commands will run as root. Only allow packages you trust." +
                "<br/>This decision applies to the command shown below.</font_size>"));

            AddContent(CreateLabel(contentBox,
                "<font_size=20>" + CommandMarkup(prompt.Command, prompt.CommandTruncated) +
                "</font_size>"));

            var buttons = Interop.elm_box_add(contentBox);
            Interop.elm_box_horizontal_set(buttons, 1);
            Interop.elm_box_padding_set(buttons, 12, 8);
            Interop.evas_object_size_hint_weight_set(buttons, 1.0, 0.0);
            Interop.evas_object_size_hint_align_set(buttons, -1.0, 1.0);
            Interop.elm_box_pack_end(contentBox, buttons);
            Interop.evas_object_show(buttons);
            contentObjects.Add(buttons);

            var deny = CreateButton(buttons, "Deny", DecisionDelegate, (IntPtr)1);
            CreateButton(buttons, "Allow once", DecisionDelegate, (IntPtr)2);
            CreateButton(buttons, "Always allow", DecisionDelegate, (IntPtr)3);
            Interop.elm_object_focus_set(deny, 1);
        }

        private static void RenderManager()
        {
            if (contentBox == IntPtr.Zero)
                return;
            ClearContent();
            SetHeader("Root access manager");
            SetStatus(managerMessage ?? "Applications with persistent root access are listed below.");

            if (!connected)
            {
                AddContent(CreateLabel(contentBox,
                    "<font_size=22><color=#ffcc66>Not connected to tv-authd. Retrying automatically.</color></font_size>"));
            }

            if (grants.Count == 0)
            {
                AddContent(CreateLabel(contentBox,
                    "<font_size=24>No application currently has persistent root access.</font_size>"));
            }
            else
            {
                var firstButton = IntPtr.Zero;
                foreach (var grant in grants.Take(5))
                {
                    var row = Interop.elm_box_add(contentBox);
                    Interop.elm_box_horizontal_set(row, 1);
                    Interop.elm_box_padding_set(row, 12, 8);
                    Interop.evas_object_size_hint_weight_set(row, 1.0, 0.0);
                    Interop.evas_object_size_hint_align_set(row, -1.0, 0.5);
                    Interop.elm_box_pack_end(contentBox, row);
                    Interop.evas_object_show(row);
                    contentObjects.Add(row);

                    AddToBox(row, CreateLabel(row,
                        "<font_size=24><b>" + Escape(grant.Package) + "</b></font_size>" +
                        "<br/><font_size=17><color=#b0b0b0>" + Escape(grant.Label) +
                        "</color></font_size>"));

                    var handle = GCHandle.Alloc(grant.Label);
                    revokeHandles.Add(GCHandle.ToIntPtr(handle));
                    var button = CreateButton(
                        row,
                        "Revoke",
                        RevokeDelegate,
                        GCHandle.ToIntPtr(handle));
                    if (firstButton == IntPtr.Zero)
                        firstButton = button;
                }

                if (grants.Count > 5)
                {
                    AddContent(CreateLabel(contentBox,
                        "<font_size=18>Only the first 5 grants are shown.</font_size>"));
                }
                if (firstButton != IntPtr.Zero)
                    Interop.elm_object_focus_set(firstButton, 1);
            }

            var buttons = Interop.elm_box_add(contentBox);
            Interop.elm_box_horizontal_set(buttons, 1);
            Interop.elm_box_padding_set(buttons, 12, 8);
            Interop.evas_object_size_hint_weight_set(buttons, 1.0, 0.0);
            Interop.evas_object_size_hint_align_set(buttons, -1.0, 1.0);
            Interop.elm_box_pack_end(contentBox, buttons);
            Interop.evas_object_show(buttons);
            contentObjects.Add(buttons);
            var refresh = CreateButton(buttons, "Refresh", RefreshDelegate, IntPtr.Zero);
            if (grants.Count == 0)
                Interop.elm_object_focus_set(refresh, 1);
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
            Interop.evas_object_size_hint_min_set(button, 240, 84);
            Interop.evas_object_smart_callback_add(button, Clicked, callback, data);
            Interop.elm_box_pack_end(parent, button);
            Interop.evas_object_show(button);
            return button;
        }

        private static void ClearContent()
        {
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

        private static void SetHeader(string text)
        {
            SetText(headerLabel, "<font_size=42><b>" + Escape(text) + "</b></font_size>");
        }

        private static void SetStatus(string text)
        {
            SetText(statusLabel, "<font_size=22>" + Escape(text) + "</font_size>");
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
                while (line.Length > 76)
                {
                    lines.Add(line.Substring(0, 76));
                    line = line.Substring(76);
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
                builder.Append("<br/><i>Command truncated for display.</i>");
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

        private static void SetHighestNotificationLevel()
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
                var result = Interop.efl_util_set_notification_window_level(window, item.Item2);
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
                    if (info.Exists && info.Length > 64 * 1024)
                        File.Delete(LogPath);
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
