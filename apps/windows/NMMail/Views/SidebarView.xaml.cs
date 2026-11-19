using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NMMail.Models;
using NMMail.ViewModels;

namespace NMMail.Views;

public sealed partial class SidebarView : UserControl
{
    public SidebarView()
    {
        InitializeComponent();
    }

    public MainViewModel ViewModel { get; set; } = null!;

    public event EventHandler? SettingsRequested;

    private void FolderList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is SidebarItem item) _ = ViewModel.OnSidebarItemInvokedAsync(item);
    }

    private void Chevron_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is SidebarItem item) ViewModel.ToggleExpanded(item);
    }

    private void Reauth_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is SidebarItem item) ViewModel.RequestReauth(item);
    }

    private void NewMail_Click(object sender, RoutedEventArgs e) => ViewModel.Compose(ComposeModes.New);

    private void Settings_Click(object sender, RoutedEventArgs e) => SettingsRequested?.Invoke(this, EventArgs.Empty);
}
