using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Input;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;

namespace NMMail.Views;

public sealed partial class MessageListView : UserControl
{
    private readonly CollectionViewSource _source = new() { IsSourceGrouped = true };
    private ScrollViewer? _scrollViewer;
    private IReadOnlyList<MessageItem> _menuTargets = [];

    public MessageListView()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }

    public MainViewModel ViewModel { get; private set; } = null!;

    public event EventHandler? AddAccountRequested;

    public void Initialize(MainViewModel viewModel)
    {
        ViewModel = viewModel;
        _source.Source = viewModel.Groups;
        MessageList.ItemsSource = _source.View;
        viewModel.SelectRequested += OnSelectRequested;
        viewModel.ListReloaded += (_, _) => DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, CheckLoadMore);
    }

    public void FocusList()
    {
        if (MessageList.SelectedItem is { } selected && MessageList.ContainerFromItem(selected) is Control container)
            container.Focus(FocusState.Keyboard);
        else
            MessageList.Focus(FocusState.Keyboard);
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (_scrollViewer is not null) return;
        _scrollViewer = VisualTree.FindDescendant<ScrollViewer>(MessageList);
        if (_scrollViewer is not null) _scrollViewer.ViewChanged += (_, _) => CheckLoadMore();
    }

    /// <summary>Incremental loading: ask for the next page when the end of the list is about one screen away.</summary>
    private void CheckLoadMore()
    {
        var sv = _scrollViewer;
        if (sv is null || ViewModel is null) return;
        if (sv.ScrollableHeight - sv.VerticalOffset <= sv.ViewportHeight) _ = ViewModel.LoadMoreAsync();
    }

    // ------------------------------------------------------------------ selection

    private void MessageList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var selected = new List<MessageItem>(MessageList.SelectedItems.Count);
        foreach (var o in MessageList.SelectedItems)
        {
            if (o is MessageItem m) selected.Add(m);
        }
        ViewModel.OnSelectionChanged(selected);
    }

    private void OnSelectRequested(object? sender, MessageItem? item)
    {
        if (item is null)
        {
            MessageList.SelectedItems.Clear();
            return;
        }
        var hadFocus = FocusManager.GetFocusedElement(XamlRoot) is DependencyObject focused && VisualTree.IsDescendantOf(focused, MessageList);
        if (!ReferenceEquals(MessageList.SelectedItem, item) || MessageList.SelectedItems.Count != 1)
            MessageList.SelectedItem = item;
        MessageList.ScrollIntoView(item);
        if (hadFocus)
        {
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
            {
                if (MessageList.ContainerFromItem(item) is Control container) container.Focus(FocusState.Keyboard);
            });
        }
    }

    private void MessageList_ContainerContentChanging(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        // A recycled container must not keep the hover state of its previous item.
        if (args.InRecycleQueue && args.Item is MessageItem item) item.IsHovered = false;
    }

    // ------------------------------------------------------------------ hover quick actions

    private void Row_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (e.Pointer.PointerDeviceType == PointerDeviceType.Touch) return;
        if ((sender as FrameworkElement)?.DataContext is MessageItem item) item.IsHovered = true;
    }

    private void Row_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is MessageItem item) item.IsHovered = false;
    }

    private static MessageItem? ItemOf(object sender) => (sender as FrameworkElement)?.Tag as MessageItem;

    private void QuickArchive_Click(object sender, RoutedEventArgs e)
    {
        if (ItemOf(sender) is { } item) _ = ViewModel.ArchiveAsync([item]);
    }

    private void QuickDelete_Click(object sender, RoutedEventArgs e)
    {
        if (ItemOf(sender) is { } item) _ = ViewModel.DeleteAsync([item]);
    }

    private void QuickRead_Click(object sender, RoutedEventArgs e)
    {
        if (ItemOf(sender) is { } item) _ = ViewModel.SetSeenAsync([item], item.IsUnread);
    }

    private void QuickFlag_Click(object sender, RoutedEventArgs e)
    {
        if (ItemOf(sender) is { } item) _ = ViewModel.ToggleFlagAsync([item]);
    }

    // ------------------------------------------------------------------ context menu

    private void Row_ContextRequested(UIElement sender, ContextRequestedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not MessageItem item) return;
        args.Handled = true;
        if (!MessageList.SelectedItems.Contains(item)) MessageList.SelectedItem = item;
        _menuTargets = ViewModel.SelectedItems.Count > 0 ? [.. ViewModel.SelectedItems] : [item];

        var menu = (MenuFlyout)Resources["RowMenu"];
        PrepareMenu(menu);
        if (args.TryGetPosition(sender, out var point))
            menu.ShowAt(sender, new FlyoutShowOptions { Position = point });
        else
            menu.ShowAt((FrameworkElement)sender);
    }

    private void PrepareMenu(MenuFlyout menu)
    {
        var anyUnread = _menuTargets.Any(i => i.IsUnread);
        var anyRead = _menuTargets.Any(i => !i.IsUnread);
        var single = _menuTargets.Count == 1;
        // Items are identified by position (see RowMenu in XAML).
        menu.Items[0].IsEnabled = single;
        menu.Items[1].IsEnabled = single;
        menu.Items[2].IsEnabled = single;
        menu.Items[4].Visibility = anyUnread ? Visibility.Visible : Visibility.Collapsed;
        menu.Items[5].Visibility = anyRead ? Visibility.Visible : Visibility.Collapsed;

        if (menu.Items[^1] is MenuFlyoutSubItem move)
        {
            move.Items.Clear();
            var accounts = _menuTargets.Select(i => i.AccountId).Distinct().ToList();
            move.IsEnabled = accounts.Count == 1;
            if (accounts.Count != 1) return;
            foreach (var folder in ViewModel.MoveTargets(accounts[0]))
            {
                var id = folder.Folder!.Id;
                var item = new MenuFlyoutItem
                {
                    Text = new string(' ', folder.Depth * 3) + folder.Title,
                    Icon = new FontIcon { Glyph = folder.Glyph },
                    Tag = id,
                };
                item.Click += MenuMoveTo_Click;
                move.Items.Add(item);
            }
        }
    }

    private void MenuReply_Click(object sender, RoutedEventArgs e) => ViewModel.Compose(ComposeModes.Reply);

    private void MenuReplyAll_Click(object sender, RoutedEventArgs e) => ViewModel.Compose(ComposeModes.ReplyAll);

    private void MenuForward_Click(object sender, RoutedEventArgs e) => ViewModel.Compose(ComposeModes.Forward);

    private void MenuRead_Click(object sender, RoutedEventArgs e) => _ = ViewModel.SetSeenAsync(_menuTargets, true);

    private void MenuUnread_Click(object sender, RoutedEventArgs e) => _ = ViewModel.SetSeenAsync(_menuTargets, false);

    private void MenuFlag_Click(object sender, RoutedEventArgs e) => _ = ViewModel.ToggleFlagAsync(_menuTargets);

    private void MenuArchive_Click(object sender, RoutedEventArgs e) => _ = ViewModel.ArchiveAsync(_menuTargets);

    private void MenuDelete_Click(object sender, RoutedEventArgs e) => _ = ViewModel.DeleteAsync(_menuTargets);

    private void MenuMoveTo_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is long folderId) _ = ViewModel.MoveAsync(_menuTargets, folderId);
    }

    // ------------------------------------------------------------------ header

    private void ClearSearch_Click(object sender, RoutedEventArgs e) => ViewModel.ExitSearch();

    private void AddAccount_Click(object sender, RoutedEventArgs e) => AddAccountRequested?.Invoke(this, EventArgs.Empty);
}
