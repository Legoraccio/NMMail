using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Text.Json;
using Microsoft.Web.WebView2.Core;
using Microsoft.Windows.ApplicationModel.Resources;
using NMMail.Models;

namespace NMMail.Services;

public static class AppInfo
{
    public static string Version { get; } =
        typeof(AppInfo).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion.Split('+')[0]
        ?? typeof(AppInfo).Assembly.GetName().Version?.ToString(3)
        ?? "0.0.0";

    public static string LocalDataFolder { get; } =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "NMMail");

    public static string IconPath => Path.Combine(AppContext.BaseDirectory, "Assets", "nmmail.ico");

    public static void Log(string message)
    {
        try
        {
            Directory.CreateDirectory(LocalDataFolder);
            File.AppendAllText(Path.Combine(LocalDataFolder, "ui.log"),
                $"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {message}{Environment.NewLine}");
        }
        catch
        {
            // logging must never throw
        }
        Debug.WriteLine(message);
    }
}

/// <summary>Localized strings from Strings/&lt;lang&gt;/Resources.resw (MRT Core).</summary>
public static class Loc
{
    private static ResourceLoader? s_loader;

    public static string Get(string key)
    {
        try
        {
            s_loader ??= new ResourceLoader();
            var value = s_loader.GetString(key);
            return string.IsNullOrEmpty(value) ? key : value;
        }
        catch (Exception)
        {
            return key;
        }
    }

    public static string Format(string key, params object?[] args) =>
        string.Format(CultureInfo.CurrentCulture, Get(key), args);

    /// <summary>"it" or "en": the language of the resources MRT actually picked (used for compose.prepare lang).</summary>
    public static string LanguageCode => Get("App_LanguageCode") == "en" ? "en" : "it";
}

/// <summary>Small local file with window placement and cached preferences needed before the daemon answers.</summary>
public static class UiStateStore
{
    private static string FilePath => Path.Combine(AppInfo.LocalDataFolder, "ui-state.json");

    public static UiState Current { get; private set; } = new();

    /// <summary>False in demo mode: the demo reads the real state but never writes it (theme, language, layout).</summary>
    public static bool PersistPreferences { get; set; } = true;

    public static void Load()
    {
        try
        {
            if (File.Exists(FilePath))
            {
                ReadOnlySpan<byte> json = File.ReadAllBytes(FilePath);
                if (json.StartsWith("﻿"u8)) json = json[3..]; // tolerate a UTF-8 BOM (hand-edited files)
                Current = JsonSerializer.Deserialize(json, IpcJson.Default.UiState) ?? new UiState();
            }
        }
        catch (Exception ex)
        {
            AppInfo.Log("ui-state load failed: " + ex.Message);
            Current = new UiState();
        }
    }

    public static void Save()
    {
        if (!PersistPreferences) return;
        try
        {
            Directory.CreateDirectory(AppInfo.LocalDataFolder);
            var tmp = FilePath + ".tmp";
            File.WriteAllBytes(tmp, JsonSerializer.SerializeToUtf8Bytes(Current, IpcJson.Default.UiState));
            File.Move(tmp, FilePath, overwrite: true);
        }
        catch (Exception ex)
        {
            AppInfo.Log("ui-state save failed: " + ex.Message);
        }
    }
}

/// <summary>Command line: <c>--demo</c>, <c>--open &lt;messageId&gt;</c>, <c>--compose &lt;mailto-uri&gt;</c>.</summary>
public sealed class StartupOptions
{
    public bool Demo { get; init; }
    public long? OpenMessageId { get; init; }

    /// <summary>Parsed <c>--compose "mailto:…"</c> (or a bare mailto: argument).</summary>
    public MailtoData? Compose { get; init; }

    public static StartupOptions Parse(IReadOnlyList<string> args)
    {
        var demo = false;
        long? open = null;
        MailtoData? compose = null;
        for (var i = 0; i < args.Count; i++)
        {
            var a = args[i];
            if (a.Equals("--demo", StringComparison.OrdinalIgnoreCase))
            {
                demo = true;
            }
            else if (a.Equals("--open", StringComparison.OrdinalIgnoreCase) && i + 1 < args.Count &&
                     long.TryParse(args[i + 1], NumberStyles.Integer, CultureInfo.InvariantCulture, out var id))
            {
                open = id;
                i++;
            }
            else if (a.StartsWith("--open=", StringComparison.OrdinalIgnoreCase) &&
                     long.TryParse(a.AsSpan(7), NumberStyles.Integer, CultureInfo.InvariantCulture, out var id2))
            {
                open = id2;
            }
            else if (a.Equals("--compose", StringComparison.OrdinalIgnoreCase))
            {
                // "--compose" alone opens an empty composer; the URI is optional.
                compose = i + 1 < args.Count ? MailtoData.Parse(args[i + 1]) : null;
                if (compose is not null) i++;
                compose ??= new MailtoData();
            }
            else if (a.StartsWith("--compose=", StringComparison.OrdinalIgnoreCase))
            {
                compose = MailtoData.Parse(a[10..]) ?? new MailtoData();
            }
            else if (a.StartsWith("mailto:", StringComparison.OrdinalIgnoreCase))
            {
                compose = MailtoData.Parse(a);
            }
        }
        return new StartupOptions { Demo = demo, OpenMessageId = open, Compose = compose };
    }

    /// <summary>Parses a raw command line (as received by a redirected activation).</summary>
    public static StartupOptions ParseCommandLine(string? commandLine) => Parse(Split(commandLine ?? ""));

    private static List<string> Split(string commandLine)
    {
        var result = new List<string>();
        var current = new System.Text.StringBuilder();
        var quoted = false;
        foreach (var c in commandLine)
        {
            if (c == '"')
            {
                quoted = !quoted;
            }
            else if (char.IsWhiteSpace(c) && !quoted)
            {
                if (current.Length > 0)
                {
                    result.Add(current.ToString());
                    current.Clear();
                }
            }
            else
            {
                current.Append(c);
            }
        }
        if (current.Length > 0) result.Add(current.ToString());
        return result;
    }
}

/// <summary>RFC 6068 mailto: URI (to/cc/bcc/subject/body, percent-decoded as UTF-8).</summary>
public sealed class MailtoData
{
    public List<Addr> To { get; } = [];
    public List<Addr> Cc { get; } = [];
    public List<Addr> Bcc { get; } = [];
    public string Subject { get; set; } = "";
    public string Body { get; set; } = "";

    public static MailtoData? Parse(string? uri)
    {
        if (string.IsNullOrWhiteSpace(uri)) return null;
        uri = uri.Trim().Trim('"');
        if (!uri.StartsWith("mailto:", StringComparison.OrdinalIgnoreCase)) return null;
        var rest = uri[7..];
        var q = rest.IndexOf('?');
        var data = new MailtoData();
        AddAddresses(data.To, Decode(q >= 0 ? rest[..q] : rest));
        if (q < 0) return data;
        foreach (var pair in rest[(q + 1)..].Split('&', StringSplitOptions.RemoveEmptyEntries))
        {
            var eq = pair.IndexOf('=');
            var key = Decode(eq >= 0 ? pair[..eq] : pair).Trim().ToLowerInvariant();
            var value = eq >= 0 ? Decode(pair[(eq + 1)..]) : "";
            switch (key)
            {
                case "to": AddAddresses(data.To, value); break;
                case "cc": AddAddresses(data.Cc, value); break;
                case "bcc": AddAddresses(data.Bcc, value); break;
                case "subject": data.Subject = value; break;
                case "body": data.Body = value.Replace("\r\n", "\n"); break;
            }
        }
        return data;
    }

    private static string Decode(string value)
    {
        try
        {
            return Uri.UnescapeDataString(value);
        }
        catch (UriFormatException)
        {
            return value;
        }
    }

    private static void AddAddresses(List<Addr> list, string value)
    {
        foreach (var part in value.Split([',', ';'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var addr = Addr.Parse(part);
            if (addr.Address.Length > 0) list.Add(addr);
        }
    }
}

/// <summary>One shared WebView2 environment (user data in %LOCALAPPDATA%\NMMail\WebView2) for reader and composer.</summary>
public static class WebViewEnvironment
{
    private static Task<CoreWebView2Environment>? s_environment;

    public static string UserDataFolder { get; } = Path.Combine(AppInfo.LocalDataFolder, "WebView2");

    public static Task<CoreWebView2Environment> GetAsync() => s_environment ??= CreateAsync();

    private static async Task<CoreWebView2Environment> CreateAsync()
    {
        Directory.CreateDirectory(UserDataFolder);
        var options = new CoreWebView2EnvironmentOptions
        {
            // We never navigate inside WebView2 (links open in the browser), so background services are useless.
            AdditionalBrowserArguments = "--disable-background-networking --disable-component-update --disable-features=msSmartScreenProtection",
            AllowSingleSignOnUsingOSPrimaryAccount = false,
        };
        return await CoreWebView2Environment.CreateWithOptionsAsync("", UserDataFolder, options);
    }
}
