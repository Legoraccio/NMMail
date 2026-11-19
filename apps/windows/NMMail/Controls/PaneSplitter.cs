using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.System;

namespace NMMail.Controls;

/// <summary>
/// Thin vertical splitter that resizes <see cref="Target"/> (the column on its left).
/// Drag with the mouse/pen/touch, or focus it and use the arrow keys. Double-click raises <see cref="ToggleRequested"/>.
/// </summary>
public sealed partial class PaneSplitter : Grid
{
    private readonly Rectangle _line;
    private bool _dragging;
    private double _startX;
    private double _startWidth;

    public PaneSplitter()
    {
        Width = 6;
        Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent); // hit-testable
        IsTabStop = true;
        UseSystemFocusVisuals = true;
        _line = new Rectangle
        {
            Width = 1,
            HorizontalAlignment = HorizontalAlignment.Center,
            Opacity = 0,
        };
        Children.Add(_line);
        ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);

        Loaded += (_, _) => _line.Fill = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        PointerEntered += (_, _) => _line.Opacity = 1;
        PointerExited += (_, _) => { if (!_dragging) _line.Opacity = 0; };
        PointerPressed += OnPointerPressed;
        PointerMoved += OnPointerMoved;
        PointerReleased += OnPointerReleased;
        PointerCaptureLost += (_, _) => EndDrag();
        DoubleTapped += (_, e) =>
        {
            e.Handled = true;
            ToggleRequested?.Invoke(this, EventArgs.Empty);
        };
        KeyDown += OnKeyDown;
    }

    public ColumnDefinition? Target { get; set; }
    public double MinTargetWidth { get; set; } = 180;
    public double MaxTargetWidth { get; set; } = 720;

    /// <summary>Minimum space that must remain for the columns on the right.</summary>
    public double ReservedWidth { get; set; } = 360;

    public event EventHandler? Resized;
    public event EventHandler? ToggleRequested;

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (Target is null) return;
        _dragging = CapturePointer(e.Pointer);
        _startX = e.GetCurrentPoint(null).Position.X;
        _startWidth = Target.ActualWidth;
        _line.Opacity = 1;
        e.Handled = true;
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging || Target is null) return;
        var delta = e.GetCurrentPoint(null).Position.X - _startX;
        SetWidth(_startWidth + delta);
        e.Handled = true;
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging) return;
        ReleasePointerCapture(e.Pointer);
        EndDrag();
        e.Handled = true;
    }

    private void EndDrag()
    {
        if (!_dragging) return;
        _dragging = false;
        _line.Opacity = 0;
        Resized?.Invoke(this, EventArgs.Empty);
    }

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (Target is null) return;
        var step = e.Key switch
        {
            VirtualKey.Left => -16,
            VirtualKey.Right => 16,
            _ => 0,
        };
        if (step == 0) return;
        SetWidth(Target.ActualWidth + step);
        Resized?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private void SetWidth(double width)
    {
        if (Target is null) return;
        var max = MaxTargetWidth;
        if (Parent is FrameworkElement parent && parent.ActualWidth > 0)
        {
            // Keep room for everything that follows the target column.
            var used = 0.0;
            if (parent is Grid grid)
            {
                var index = grid.ColumnDefinitions.IndexOf(Target);
                for (var i = 0; i < index; i++) used += grid.ColumnDefinitions[i].ActualWidth;
            }
            max = Math.Min(max, parent.ActualWidth - used - ReservedWidth);
        }
        width = Math.Clamp(width, MinTargetWidth, Math.Max(MinTargetWidth, max));
        Target.Width = new GridLength(width);
    }
}
