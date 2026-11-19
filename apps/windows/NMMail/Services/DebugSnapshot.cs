#if DEBUG
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.Web.WebView2.Core;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace NMMail.Services;

/// <summary>
/// DEBUG builds only: renders every open window (XAML tree + WebView2 content + open popups/dialogs) to PNG
/// when "&lt;NMMAIL_SNAPSHOT_DIR&gt;\snap.req" appears. Used to verify the UI where screen capture is impossible
/// (e.g. a locked session, where DWM does not compose DirectComposition content).
/// </summary>
internal static class DebugSnapshot
{
    private static string? s_dir;
    private static DispatcherQueueTimer? s_timer; // must stay referenced, or the GC collects the timer
    private static bool s_busy;
    private static IEnumerable<(string Name, FrameworkElement Root, WebView2[] WebViews)> Sources()
    {
        if (App.Current.MainWindow is { } main) yield return ("main", main.SnapshotRoot, main.SnapshotWebViews);
        var i = 0;
        foreach (var compose in Views.ComposeWindow.OpenWindows.ToList())
            yield return ($"compose{i++}", compose.SnapshotRoot, compose.SnapshotWebViews);
    }

    public static void Start(DispatcherQueue dispatcher)
    {
        s_dir = Environment.GetEnvironmentVariable("NMMAIL_SNAPSHOT_DIR");
        if (string.IsNullOrEmpty(s_dir)) return;
        Directory.CreateDirectory(s_dir);
        var timer = s_timer = dispatcher.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(500);
        timer.Tick += async (_, _) =>
        {
            var request = Path.Combine(s_dir, "snap.req");
            if (s_busy || !File.Exists(request)) return;
            s_busy = true;
            try
            {
                var tag = File.ReadAllText(request).Trim();
                File.Delete(request);
                await CaptureAllAsync(string.IsNullOrEmpty(tag) ? DateTime.Now.ToString("HHmmss") : tag);
                File.WriteAllText(Path.Combine(s_dir, "snap.done"), tag);
            }
            catch (Exception ex)
            {
                File.WriteAllText(Path.Combine(s_dir, "snap.done"), "error: " + ex);
            }
            finally
            {
                s_busy = false;
            }
        };
        timer.Start();
    }

    private static async Task CaptureAllAsync(string tag)
    {
        // "hover…" tags simulate the pointer over the first row (quick actions cannot be hovered without real input).
        if (tag.StartsWith("hover", StringComparison.OrdinalIgnoreCase) &&
            App.Current.ViewModel?.Groups.FirstOrDefault()?.FirstOrDefault() is { } first)
        {
            first.IsHovered = true;
            await Task.Delay(300);
        }
        foreach (var (name, root, webViews) in Sources().ToList())
        {
            if (root.XamlRoot is null) continue;
            await CaptureAsync(root, webViews, Path.Combine(s_dir!, $"{tag}-{name}.png"));
        }
    }

    private static async Task CaptureAsync(FrameworkElement root, WebView2[] webViews, string path)
    {
        var scale = root.XamlRoot.RasterizationScale;
        var rtb = new RenderTargetBitmap();
        await rtb.RenderAsync(root);
        var width = rtb.PixelWidth;
        var height = rtb.PixelHeight;
        var pixels = (await rtb.GetPixelsAsync()).ToArray();

        // Background approximating Mica (XAML renders it transparent).
        var dark = root.ActualTheme == ElementTheme.Dark;
        byte bg = dark ? (byte)0x20 : (byte)0xF3;
        for (var i = 0; i < pixels.Length; i += 4)
        {
            var a = pixels[i + 3] / 255.0;
            pixels[i] = (byte)(pixels[i] + bg * (1 - a));
            pixels[i + 1] = (byte)(pixels[i + 1] + bg * (1 - a));
            pixels[i + 2] = (byte)(pixels[i + 2] + bg * (1 - a));
            pixels[i + 3] = 255;
        }

        foreach (var web in webViews)
        {
            if (web.CoreWebView2 is null || web.ActualWidth < 1 || !IsShown(web)) continue;
            using var stream = new InMemoryRandomAccessStream();
            await web.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, stream);
            stream.Seek(0);
            var decoder = await BitmapDecoder.CreateAsync(stream);
            var frame = await decoder.GetPixelDataAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied,
                new BitmapTransform(), ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage);
            var web8 = frame.DetachPixelData();
            var origin = web.TransformToVisual(root).TransformPoint(new Windows.Foundation.Point(0, 0));
            Blit(pixels, width, height, web8, (int)decoder.PixelWidth, (int)decoder.PixelHeight,
                (int)Math.Round(origin.X * scale), (int)Math.Round(origin.Y * scale));
        }

        // Popups (ContentDialog, flyouts, AutoSuggestBox lists) live outside the root's subtree.
        foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(root.XamlRoot))
        {
            if (popup.Child is not FrameworkElement child || child.ActualWidth < 1) continue;
            var popupRtb = new RenderTargetBitmap();
            await popupRtb.RenderAsync(child);
            var popupPixels = (await popupRtb.GetPixelsAsync()).ToArray();
            var origin = child.TransformToVisual(root).TransformPoint(new Windows.Foundation.Point(0, 0));
            BlendOver(pixels, width, height, popupPixels, popupRtb.PixelWidth, popupRtb.PixelHeight,
                (int)Math.Round(origin.X * scale), (int)Math.Round(origin.Y * scale));
        }

        using var file = File.Create(path);
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, file.AsRandomAccessStream());
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied, (uint)width, (uint)height, 96 * scale, 96 * scale, pixels);
        await encoder.FlushAsync();
    }

    private static bool IsShown(FrameworkElement element)
    {
        for (DependencyObject? e = element; e is not null; e = VisualTreeHelper.GetParent(e))
        {
            if (e is UIElement { Visibility: Visibility.Collapsed }) return false;
        }
        return true;
    }

    private static void Blit(byte[] dst, int dw, int dh, byte[] src, int sw, int sh, int ox, int oy)
    {
        for (var y = 0; y < sh; y++)
        {
            var dy = oy + y;
            if (dy < 0 || dy >= dh) continue;
            for (var x = 0; x < sw; x++)
            {
                var dx = ox + x;
                if (dx < 0 || dx >= dw) continue;
                System.Buffer.BlockCopy(src, (y * sw + x) * 4, dst, (dy * dw + dx) * 4, 4);
            }
        }
    }

    private static void BlendOver(byte[] dst, int dw, int dh, byte[] src, int sw, int sh, int ox, int oy)
    {
        for (var y = 0; y < sh; y++)
        {
            var dy = oy + y;
            if (dy < 0 || dy >= dh) continue;
            for (var x = 0; x < sw; x++)
            {
                var dx = ox + x;
                if (dx < 0 || dx >= dw) continue;
                var s = (y * sw + x) * 4;
                var d = (dy * dw + dx) * 4;
                var a = src[s + 3] / 255.0;
                for (var c = 0; c < 3; c++) dst[d + c] = (byte)Math.Min(255, src[s + c] + dst[d + c] * (1 - a));
            }
        }
    }
}
#endif
