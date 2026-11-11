using System;
using System.IO;
using System.Runtime.InteropServices;

namespace TvAuthEflPopup
{
    internal static class Program
    {
        private const int ElmWinNotification = 11;
        private const int EcoreWl2WindowTypeNotification = 5;
        private const int EextCallbackBack = 0;
        private const int NotificationTop = 4;
        private const int NotificationHigh = 3;
        private const int NotificationMedium = 2;
        private const int NotificationDefault = 1;

        private static IntPtr window;
        private static bool backGrabbed;

        [StructLayout(LayoutKind.Sequential)]
        private struct UiAppLifecycle
        {
            public IntPtr create;
            public IntPtr terminate;
            public IntPtr pause;
            public IntPtr resume;
            public IntPtr appControl;
        }

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        private delegate bool AppCreateCb(IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        private delegate void AppLifecycleCb(IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        private delegate void AppControlCb(IntPtr appControl, IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        private delegate void EvasSmartCb(IntPtr data, IntPtr obj, IntPtr eventInfo);

        [DllImport("libcapi-appfw-application.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern int ui_app_main(int argc, IntPtr argv, ref UiAppLifecycle callbacks, IntPtr userData);

        [DllImport("libcapi-appfw-application.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern void ui_app_exit();

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_win_add(IntPtr parent, IntPtr name, int type);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_title_set(IntPtr obj, IntPtr title);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_resize_object_add(IntPtr obj, IntPtr child);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_center(IntPtr obj, byte horizontal, byte vertical);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_activate(IntPtr obj);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_raise(IntPtr obj);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_modal_set(IntPtr obj, byte modal);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_win_floating_mode_set(IntPtr obj, byte floating);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_win_wl_window_get(IntPtr obj);

        [DllImport("libecore_wl2.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void ecore_wl2_window_type_set(IntPtr window, int type);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_bg_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_box_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_box_padding_set(IntPtr obj, int horizontal, int vertical);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_box_pack_end(IntPtr obj, IntPtr child);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_label_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr elm_button_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_object_part_text_set(IntPtr obj, IntPtr part, IntPtr text);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void elm_object_focus_set(IntPtr obj, byte focus);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_size_hint_weight_set(IntPtr obj, double x, double y);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_size_hint_align_set(IntPtr obj, double x, double y);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_size_hint_min_set(IntPtr obj, int width, int height);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_resize(IntPtr obj, int width, int height);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_show(IntPtr obj);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_del(IntPtr obj);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        private static extern void evas_object_smart_callback_add(IntPtr obj, IntPtr ev, EvasSmartCb callback, IntPtr data);

        [DllImport("libcapi-ui-efl-util.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern int efl_util_set_notification_window_level(IntPtr win, int level);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern void eext_object_event_callback_add(IntPtr obj, int type, EvasSmartCb callback, IntPtr data);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern byte eext_win_keygrab_set(IntPtr obj, IntPtr key);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        private static extern byte eext_win_keygrab_unset(IntPtr obj, IntPtr key);

        private static readonly AppCreateCb AppCreateDelegate = AppCreate;
        private static readonly AppLifecycleCb AppTerminateDelegate = AppTerminate;
        private static readonly AppLifecycleCb AppPauseDelegate = AppPause;
        private static readonly AppLifecycleCb AppResumeDelegate = AppResume;
        private static readonly AppControlCb AppControlDelegate = AppControl;
        private static readonly EvasSmartCb DismissDelegate = Dismiss;

        private static readonly IntPtr AppId = Ansi("org.tvauth.spike.popupdotnet");
        private static readonly IntPtr WindowTitle = Ansi("Harmless .NET/EFL test");
        private static readonly IntPtr DeleteRequest = Ansi("delete,request");
        private static readonly IntPtr Clicked = Ansi("clicked");
        private static readonly IntPtr BackKey = Ansi("XF86Back");
        private static readonly IntPtr ButtonText = Ansi("OK / Dismiss");
        private static readonly IntPtr WarningText = Ansi(
            "<align=center><font_size=42><b>Harmless test</b></font_size><br/><br/>" +
            "This is a harmless .NET/EFL approval-popup spike.<br/>" +
            "No root access is requested. No commands are run and no settings are changed.<br/><br/>" +
            "Press OK / Dismiss or RETURN to exit.</align>");

        private static int Main(string[] args)
        {
            var callbacks = new UiAppLifecycle
            {
                create = Marshal.GetFunctionPointerForDelegate(AppCreateDelegate),
                terminate = Marshal.GetFunctionPointerForDelegate(AppTerminateDelegate),
                pause = Marshal.GetFunctionPointerForDelegate(AppPauseDelegate),
                resume = Marshal.GetFunctionPointerForDelegate(AppResumeDelegate),
                appControl = Marshal.GetFunctionPointerForDelegate(AppControlDelegate),
            };

            var argv = AllocateArgv(args);
            try
            {
                return ui_app_main(args.Length, argv, ref callbacks, IntPtr.Zero);
            }
            finally
            {
                FreeArgv(argv, args.Length);
            }
        }

        private static bool AppCreate(IntPtr userData)
        {
            Log("app_create start");
            window = elm_win_add(IntPtr.Zero, AppId, ElmWinNotification);
            if (window == IntPtr.Zero)
            {
                Log("elm_win_add failed");
                return false;
            }
            Log($"window=0x{window.ToInt64():x}");

            var waylandWindow = elm_win_wl_window_get(window);
            Log($"wayland_window=0x{waylandWindow.ToInt64():x}");
            if (waylandWindow != IntPtr.Zero)
            {
                ecore_wl2_window_type_set(waylandWindow, EcoreWl2WindowTypeNotification);
                Log("set Wayland notification type");
            }

            elm_win_title_set(window, WindowTitle);
            elm_win_modal_set(window, 1);
            elm_win_floating_mode_set(window, 1);
            SetHighestNotificationLevel();

            evas_object_smart_callback_add(window, DeleteRequest, DismissDelegate, IntPtr.Zero);
            eext_object_event_callback_add(window, EextCallbackBack, DismissDelegate, IntPtr.Zero);
            backGrabbed = eext_win_keygrab_set(window, BackKey) == 1;
            if (!backGrabbed)
                Console.Error.WriteLine("efl-popup: XF86Back keygrab failed; button remains available");

            var background = elm_bg_add(window);
            if (background == IntPtr.Zero) return false;
            evas_object_size_hint_weight_set(background, 1.0, 1.0);
            elm_win_resize_object_add(window, background);
            evas_object_show(background);

            var box = elm_box_add(window);
            if (box == IntPtr.Zero) return false;
            evas_object_size_hint_weight_set(box, 1.0, 1.0);
            evas_object_size_hint_align_set(box, -1.0, -1.0);
            elm_box_padding_set(box, 32, 32);
            elm_win_resize_object_add(window, box);
            evas_object_show(box);

            var label = elm_label_add(window);
            if (label == IntPtr.Zero) return false;
            elm_object_part_text_set(label, IntPtr.Zero, WarningText);
            evas_object_size_hint_weight_set(label, 1.0, 1.0);
            evas_object_size_hint_align_set(label, -1.0, -1.0);
            elm_box_pack_end(box, label);
            evas_object_show(label);

            var button = elm_button_add(window);
            if (button == IntPtr.Zero) return false;
            elm_object_part_text_set(button, IntPtr.Zero, ButtonText);
            evas_object_size_hint_min_set(button, 320, 96);
            evas_object_smart_callback_add(button, Clicked, DismissDelegate, IntPtr.Zero);
            elm_box_pack_end(box, button);
            evas_object_show(button);
            elm_object_focus_set(button, 1);

            evas_object_resize(window, 1200, 650);
            elm_win_center(window, 1, 1);
            evas_object_show(window);
            elm_win_raise(window);
            elm_win_activate(window);
            Log("window shown/raised/activated");
            return true;
        }

        private static void AppTerminate(IntPtr userData)
        {
            if (window == IntPtr.Zero) return;
            if (backGrabbed)
                eext_win_keygrab_unset(window, BackKey);
            evas_object_del(window);
            window = IntPtr.Zero;
        }

        private static void AppPause(IntPtr userData) { }
        private static void AppResume(IntPtr userData) { }
        private static void AppControl(IntPtr appControl, IntPtr userData) { }
        private static void Dismiss(IntPtr data, IntPtr obj, IntPtr eventInfo) => ui_app_exit();

        private static void SetHighestNotificationLevel()
        {
            foreach (var item in new[]
            {
                Tuple.Create("TOP", NotificationTop),
                Tuple.Create("HIGH", NotificationHigh),
                Tuple.Create("MEDIUM", NotificationMedium),
                Tuple.Create("DEFAULT", NotificationDefault),
            })
            {
                var result = efl_util_set_notification_window_level(window, item.Item2);
                if (result == 0)
                {
                    Log($"notification level {item.Item1} accepted");
                    return;
                }
                Log($"notification level {item.Item1} rejected with {result}");
            }
            Console.Error.WriteLine("efl-popup: using platform-default notification level");
        }

        private static void Log(string message)
        {
            try
            {
                File.AppendAllText("/tmp/tvauth-popupdotnet.log",
                    $"{DateTime.UtcNow:O} {message}{Environment.NewLine}");
            }
            catch
            {
                // Diagnostics must never prevent the harmless UI test.
            }
        }

        private static IntPtr Ansi(string value) => Marshal.StringToHGlobalAnsi(value);

        private static IntPtr AllocateArgv(string[] args)
        {
            var argv = Marshal.AllocHGlobal(IntPtr.Size * (args.Length + 1));
            for (var i = 0; i < args.Length; i++)
                Marshal.WriteIntPtr(argv, i * IntPtr.Size, Ansi(args[i]));
            Marshal.WriteIntPtr(argv, args.Length * IntPtr.Size, IntPtr.Zero);
            return argv;
        }

        private static void FreeArgv(IntPtr argv, int count)
        {
            for (var i = 0; i < count; i++)
            {
                var value = Marshal.ReadIntPtr(argv, i * IntPtr.Size);
                if (value != IntPtr.Zero) Marshal.FreeHGlobal(value);
            }
            Marshal.FreeHGlobal(argv);
        }
    }
}
