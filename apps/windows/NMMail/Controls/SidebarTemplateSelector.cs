using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NMMail.ViewModels;

namespace NMMail.Controls;

public sealed partial class SidebarTemplateSelector : DataTemplateSelector
{
    public DataTemplate? UnifiedTemplate { get; set; }
    public DataTemplate? AccountTemplate { get; set; }
    public DataTemplate? FolderTemplate { get; set; }

    protected override DataTemplate? SelectTemplateCore(object item) => item is SidebarItem sidebarItem
        ? sidebarItem.Kind switch
        {
            SidebarKind.Unified => UnifiedTemplate,
            SidebarKind.Account => AccountTemplate,
            _ => FolderTemplate,
        }
        : null;

    protected override DataTemplate? SelectTemplateCore(object item, DependencyObject container) => SelectTemplateCore(item);
}
