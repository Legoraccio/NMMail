using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using NMMail.Models;

namespace NMMail.Services;

/// <summary>Applies the theme setting (system/light/dark) live to every open window.</summary>
public static class ThemeHelper
{
    private static readonly List<WeakReference<Window>> s_windows = [];

    public static string Current { get; private set; } = ThemeNames.System;

    public static void Register(Window window)
    {
        s_windows.Add(new WeakReference<Window>(window));
        window.Closed += (_, _) => s_windows.RemoveAll(w => !w.TryGetTarget(out var target) || ReferenceEquals(target, window));
        ApplyTo(window, Current);
    }

    public static void Apply(string theme)
    {
        Current = theme;
        if (UiStateStore.PersistPreferences && UiStateStore.Current.Theme != theme)
        {
            UiStateStore.Current.Theme = theme;
            UiStateStore.Save();
        }
        foreach (var weak in s_windows.ToList())
        {
            if (weak.TryGetTarget(out var window)) ApplyTo(window, theme);
        }
    }

    private static void ApplyTo(Window window, string theme)
    {
        if (window.Content is FrameworkElement root)
        {
            root.RequestedTheme = theme switch
            {
                ThemeNames.Light => ElementTheme.Light,
                ThemeNames.Dark => ElementTheme.Dark,
                _ => ElementTheme.Default,
            };
        }
        try
        {
            window.AppWindow.TitleBar.PreferredTheme = theme switch
            {
                ThemeNames.Light => TitleBarTheme.Light,
                ThemeNames.Dark => TitleBarTheme.Dark,
                _ => TitleBarTheme.UseDefaultAppMode,
            };
        }
        catch (Exception ex)
        {
            AppInfo.Log("title bar theme: " + ex.Message);
        }
    }
}
