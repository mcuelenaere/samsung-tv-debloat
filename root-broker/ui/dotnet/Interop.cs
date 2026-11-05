using System;
using System.Runtime.InteropServices;

namespace TvAuthUi
{
    /// <summary>
    /// The complete native surface used by the approval UI. Keep this small:
    /// all protocol and view logic belongs in managed code.
    /// </summary>
    internal static class Interop
    {
        public const int ElmWinNotification = 11;
        public const int EcoreWl2WindowTypeNotification = 5;
        public const int EextCallbackBack = 0;
        public const int NotificationTop = 4;
        public const int NotificationHigh = 3;
        public const int NotificationMedium = 2;
        public const int NotificationDefault = 1;
        public const int ElmWrapWord = 2;

        [StructLayout(LayoutKind.Sequential)]
        public struct UiAppLifecycle
        {
            public IntPtr create;
            public IntPtr terminate;
            public IntPtr pause;
            public IntPtr resume;
            public IntPtr appControl;
        }

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        public delegate bool AppCreateCb(IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        public delegate void AppLifecycleCb(IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        public delegate void AppControlCb(IntPtr appControl, IntPtr userData);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        public delegate void EvasSmartCb(IntPtr data, IntPtr obj, IntPtr eventInfo);

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        public delegate bool EcoreTimerCb(IntPtr data);

        [DllImport("libcapi-appfw-application.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern int ui_app_main(int argc, IntPtr argv, ref UiAppLifecycle callbacks, IntPtr userData);

        [DllImport("libcapi-appfw-application.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern void ui_app_exit();

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_win_add(IntPtr parent, IntPtr name, int type);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_title_set(IntPtr obj, IntPtr title);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_resize_object_add(IntPtr obj, IntPtr child);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_center(IntPtr obj, byte horizontal, byte vertical);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_activate(IntPtr obj);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_raise(IntPtr obj);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_modal_set(IntPtr obj, byte modal);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_floating_mode_set(IntPtr obj, byte floating);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_win_wl_window_get(IntPtr obj);

        [DllImport("libecore_wl2.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void ecore_wl2_window_type_set(IntPtr window, int type);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_alpha_set(IntPtr obj, byte alpha);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_win_screen_size_get(IntPtr obj, out int x, out int y, out int w, out int h);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_table_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_table_pack(IntPtr obj, IntPtr child, int col, int row, int colspan, int rowspan);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_box_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_box_horizontal_set(IntPtr obj, byte horizontal);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_box_padding_set(IntPtr obj, int horizontal, int vertical);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_box_pack_end(IntPtr obj, IntPtr child);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_label_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_label_line_wrap_set(IntPtr obj, int wrap);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr elm_button_add(IntPtr parent);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_object_part_text_set(IntPtr obj, IntPtr part, IntPtr text);

        [DllImport("libelementary.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void elm_object_focus_set(IntPtr obj, byte focus);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_size_hint_weight_set(IntPtr obj, double x, double y);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_size_hint_align_set(IntPtr obj, double x, double y);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_size_hint_min_set(IntPtr obj, int width, int height);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_size_hint_padding_set(IntPtr obj, int left, int right, int top, int bottom);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr evas_object_evas_get(IntPtr obj);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr evas_object_rectangle_add(IntPtr evas);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_color_set(IntPtr obj, int r, int g, int b, int a);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_resize(IntPtr obj, int width, int height);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_show(IntPtr obj);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_del(IntPtr obj);

        [DllImport("libevas.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern void evas_object_smart_callback_add(IntPtr obj, IntPtr ev, EvasSmartCb callback, IntPtr data);

        [DllImport("libcapi-ui-efl-util.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern int efl_util_set_notification_window_level(IntPtr win, int level);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern void eext_object_event_callback_add(IntPtr obj, int type, EvasSmartCb callback, IntPtr data);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern byte eext_win_keygrab_set(IntPtr obj, IntPtr key);

        [DllImport("libefl-extension.so.0", CallingConvention = CallingConvention.Cdecl)]
        public static extern byte eext_win_keygrab_unset(IntPtr obj, IntPtr key);

        [DllImport("libecore.so.1", CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr ecore_timer_add(double interval, EcoreTimerCb callback, IntPtr data);

        public static IntPtr Ansi(string value) => Marshal.StringToHGlobalAnsi(value);
    }
}
