using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NMMail.Models;
using NMMail.Services;

namespace NMMail.Views;

/// <summary>"Altro (IMAP)" account form. Stays open (with progress) until accounts.addImap succeeds.</summary>
public sealed partial class ImapAccountDialog : ContentDialog
{
    private static readonly string[] s_security = [ConnectionSecurity.Tls, ConnectionSecurity.StartTls, ConnectionSecurity.None];
    private static readonly int[] s_imapPorts = [993, 143, 143];
    private static readonly int[] s_smtpPorts = [465, 587, 25];

    private readonly IDaemonClient _client;
    private bool _hostsEdited;
    private bool _updatingHosts;

    public ImapAccountDialog(IDaemonClient client, Account? existing = null)
    {
        _client = client;
        InitializeComponent();
        ErrorBar.IsOpen = false;
        if (existing is not null)
        {
            Title = Loc.Get("Imap_ReauthTitle");
            EmailBox.Text = existing.Email;
            NameBox.Text = existing.DisplayName;
        }
    }

    public Account? Result { get; private set; }

    private void EmailBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_hostsEdited) return;
        var email = EmailBox.Text.Trim();
        var at = email.IndexOf('@');
        if (at < 0 || at == email.Length - 1) return;
        var domain = email[(at + 1)..];
        _updatingHosts = true;
        ImapHostBox.Text = "imap." + domain;
        SmtpHostBox.Text = "smtp." + domain;
        _updatingHosts = false;
    }

    private void Host_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (!_updatingHosts) _hostsEdited = true;
    }

    private void Security_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is not ComboBox box || box.SelectedIndex < 0) return;
        var imap = ReferenceEquals(box, ImapSecurityBox);
        var portBox = imap ? ImapPortBox : SmtpPortBox;
        if (portBox is null) return;
        var defaults = imap ? s_imapPorts : s_smtpPorts;
        // Only replace a port the user did not customize (e.g. 1143 for the local test server stays).
        if (double.IsNaN(portBox.Value) || defaults.Contains((int)portBox.Value)) portBox.Value = defaults[box.SelectedIndex];
    }

    private async void OnPrimaryButtonClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true; // closed manually on success
        if (!TryBuild(out var request, out var error))
        {
            ShowError(error);
            return;
        }
        SetBusy(true);
        try
        {
            Result = await _client.AddImapAccountAsync(request);
            Hide();
        }
        catch (Exception ex)
        {
            ShowError(ex.Message);
        }
        finally
        {
            SetBusy(false);
        }
    }

    private bool TryBuild(out AddImapParams request, out string error)
    {
        request = new AddImapParams();
        error = "";
        var email = EmailBox.Text.Trim();
        if (email.Length < 3 || !email.Contains('@'))
        {
            error = Loc.Get("Imap_ErrorEmail");
            return false;
        }
        if (PasswordInput.Password.Length == 0)
        {
            error = Loc.Get("Imap_ErrorPassword");
            return false;
        }
        if (string.IsNullOrWhiteSpace(ImapHostBox.Text) || string.IsNullOrWhiteSpace(SmtpHostBox.Text))
        {
            error = Loc.Get("Imap_ErrorHost");
            return false;
        }
        if (double.IsNaN(ImapPortBox.Value) || double.IsNaN(SmtpPortBox.Value))
        {
            error = Loc.Get("Imap_ErrorPort");
            return false;
        }
        request = new AddImapParams
        {
            Email = email,
            DisplayName = string.IsNullOrWhiteSpace(NameBox.Text) ? email : NameBox.Text.Trim(),
            Username = string.IsNullOrWhiteSpace(UserBox.Text) ? null : UserBox.Text.Trim(),
            Password = PasswordInput.Password,
            ImapHost = ImapHostBox.Text.Trim(),
            ImapPort = (int)ImapPortBox.Value,
            ImapSecurity = s_security[Math.Max(0, ImapSecurityBox.SelectedIndex)],
            SmtpHost = SmtpHostBox.Text.Trim(),
            SmtpPort = (int)SmtpPortBox.Value,
            SmtpSecurity = s_security[Math.Max(0, SmtpSecurityBox.SelectedIndex)],
        };
        return true;
    }

    private void ShowError(string message)
    {
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
    }

    private void SetBusy(bool busy)
    {
        Progress.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        Progress.IsIndeterminate = busy;
        IsPrimaryButtonEnabled = !busy;
        if (busy) ErrorBar.IsOpen = false;
    }
}
