using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Threading;

namespace TvAuthUi
{
    internal sealed class PromptInfo
    {
        public string PromptId;
        public string Nonce;
        public string RequestId;
        public string Label;
        public string Package;
        public string Command;
        public int CommandBytes;
        public bool CommandTruncated;
        public int TimeoutMs;
    }

    internal sealed class GrantInfo
    {
        public string Label;
        public string Package;
        public long GrantedAt;
    }

    /// <summary>
    /// Direct, in-process client for tv-authd. This deliberately uses the
    /// standard-library Unix socket support and a blocking reader thread.
    /// </summary>
    internal sealed class BrokerClient
    {
        private const string SocketPath = "/run/tv-auth/broker.sock";
        private readonly ConcurrentQueue<Action> dispatch;
        private readonly Action<string> log;
        private readonly object writeLock = new object();
        private readonly Thread thread;
        private volatile bool stopped;
        private Socket socket;
        private long nextId;
        private long lastHelloId;
        private long lastDecisionId;
        private long lastGrantsId;

        public event Action<PromptInfo> PromptReceived;
        public event Action<List<GrantInfo>> GrantsReceived;
        public event Action<bool, string> DecisionFinished;
        public event Action<bool, string> RevokeFinished;
        public event Action<bool> ConnectionChanged;
        public event Action<string> StatusMessage;

        public BrokerClient(ConcurrentQueue<Action> dispatch, Action<string> log)
        {
            this.dispatch = dispatch;
            this.log = log;
            thread = new Thread(Run)
            {
                IsBackground = true,
                Name = "tv-auth-broker-client"
            };
        }

        public void Start() => thread.Start();

        public void Stop()
        {
            stopped = true;
            CloseSocket();
        }

        public void RequestGrants()
        {
            var id = Interlocked.Increment(ref nextId);
            Interlocked.Exchange(ref lastGrantsId, id);
            if (!SendRaw(JsonSerializer.Serialize(new
            {
                version = 1,
                id = "grants-" + id,
                method = "grants.list"
            })))
            {
                dispatch.Enqueue(() => StatusMessage?.Invoke("Not connected to tv-authd."));
            }
        }

        public void Revoke(string label)
        {
            var id = Interlocked.Increment(ref nextId);
            if (!SendRaw(JsonSerializer.Serialize(new
            {
                version = 1,
                id = "revoke-" + id,
                method = "grants.revoke",
                label
            })))
            {
                dispatch.Enqueue(() => RevokeFinished?.Invoke(false, "not_connected"));
            }
        }

        public void Decide(PromptInfo prompt, string decision)
        {
            var id = Interlocked.Increment(ref nextId);
            Interlocked.Exchange(ref lastDecisionId, id);
            if (!SendRaw(JsonSerializer.Serialize(new
            {
                version = 1,
                id = "decision-" + id,
                method = "ui.decision",
                prompt_id = prompt.PromptId,
                nonce = prompt.Nonce,
                decision
            })))
            {
                dispatch.Enqueue(() => DecisionFinished?.Invoke(false, "not_connected"));
            }
        }

        private void Run()
        {
            while (!stopped)
            {
                Socket connection = null;
                try
                {
                    connection = new Socket(
                        AddressFamily.Unix,
                        SocketType.Stream,
                        ProtocolType.Unspecified);
                    connection.Connect(new UnixDomainSocketEndPoint(SocketPath));
                    lock (writeLock)
                    {
                        socket = connection;
                    }

                    using (var stream = new NetworkStream(connection, false))
                    using (var reader = new StreamReader(stream, Encoding.UTF8))
                    {
                        dispatch.Enqueue(() => ConnectionChanged?.Invoke(true));
                        SendHello();
                        RequestGrants();

                        string line;
                        while (!stopped && (line = reader.ReadLine()) != null)
                        {
                            HandleLine(line);
                        }
                    }
                }
                catch (Exception error)
                {
                    if (!stopped)
                        log("broker: " + error.Message);
                }
                finally
                {
                    lock (writeLock)
                    {
                        socket = null;
                    }
                    try { connection?.Close(); } catch { }
                    dispatch.Enqueue(() => ConnectionChanged?.Invoke(false));
                }

                if (!stopped)
                    Thread.Sleep(2000);
            }
        }

        private void SendHello()
        {
            var id = Interlocked.Increment(ref nextId);
            Interlocked.Exchange(ref lastHelloId, id);
            if (!SendRaw(JsonSerializer.Serialize(new
            {
                version = 1,
                id = "hello-" + id,
                method = "ui.hello"
            })))
            {
                throw new IOException("failed to send ui.hello");
            }
        }

        private bool SendRaw(string json)
        {
            lock (writeLock)
            {
                if (socket == null)
                    return false;
                try
                {
                    var bytes = Encoding.UTF8.GetBytes(json + "\n");
                    var sent = 0;
                    while (sent < bytes.Length)
                        sent += socket.Send(bytes, sent, bytes.Length - sent, SocketFlags.None);
                    return true;
                }
                catch (Exception error)
                {
                    log("broker send: " + error.Message);
                    return false;
                }
            }
        }

        private void CloseSocket()
        {
            lock (writeLock)
            {
                try { socket?.Shutdown(SocketShutdown.Both); } catch { }
                try { socket?.Close(); } catch { }
                socket = null;
            }
        }

        private void HandleLine(string line)
        {
            JsonDocument document;
            try
            {
                document = JsonDocument.Parse(line);
            }
            catch (JsonException error)
            {
                log("broker parse: " + error.Message);
                return;
            }

            using (document)
            {
                var root = document.RootElement;
                if (root.TryGetProperty("type", out var type)
                    && type.ValueKind == JsonValueKind.String
                    && type.GetString() == "ui.prompt")
                {
                    var prompt = new PromptInfo
                    {
                        PromptId = GetString(root, "prompt_id"),
                        Nonce = GetString(root, "nonce"),
                        RequestId = GetString(root, "request_id"),
                        Label = GetString(root, "label"),
                        Package = GetString(root, "package"),
                        Command = GetString(root, "command"),
                        CommandBytes = GetInt(root, "command_bytes"),
                        CommandTruncated = GetBool(root, "command_truncated"),
                        TimeoutMs = GetInt(root, "timeout_ms")
                    };
                    dispatch.Enqueue(() => PromptReceived?.Invoke(prompt));
                    return;
                }

                var id = GetString(root, "id");
                var status = GetString(root, "status");
                var error = GetString(root, "error");

                if (root.TryGetProperty("grants", out var grants)
                    && grants.ValueKind == JsonValueKind.Array)
                {
                    var list = new List<GrantInfo>();
                    foreach (var item in grants.EnumerateArray())
                    {
                        list.Add(new GrantInfo
                        {
                            Label = GetString(item, "label"),
                            Package = GetString(item, "package"),
                            GrantedAt = GetLong(item, "granted_at")
                        });
                    }
                    dispatch.Enqueue(() => GrantsReceived?.Invoke(list));
                    return;
                }

                if (root.TryGetProperty("revoked", out var revoked)
                    && (revoked.ValueKind == JsonValueKind.True
                        || revoked.ValueKind == JsonValueKind.False))
                {
                    var value = revoked.GetBoolean();
                    dispatch.Enqueue(() => RevokeFinished?.Invoke(value, value ? "" : "not_found"));
                    if (value)
                        RequestGrants();
                    return;
                }

                if (id == "hello-" + Interlocked.Read(ref lastHelloId))
                {
                    if (status != "ok")
                        dispatch.Enqueue(() => StatusMessage?.Invoke(
                            "tv-authd rejected this UI (" + error + ")."));
                    return;
                }

                if (id == "decision-" + Interlocked.Read(ref lastDecisionId))
                {
                    dispatch.Enqueue(() => DecisionFinished?.Invoke(status == "ok", error));
                    return;
                }

                if (id == "grants-" + Interlocked.Read(ref lastGrantsId) && status != "ok")
                {
                    dispatch.Enqueue(() => StatusMessage?.Invoke(
                        "Could not load grants (" + error + ")."));
                }
            }
        }

        private static string GetString(JsonElement element, string name)
        {
            return element.TryGetProperty(name, out var value)
                && value.ValueKind == JsonValueKind.String
                ? value.GetString() ?? ""
                : "";
        }

        private static int GetInt(JsonElement element, string name)
        {
            return element.TryGetProperty(name, out var value)
                && value.ValueKind == JsonValueKind.Number
                && value.TryGetInt32(out var result)
                ? result
                : 0;
        }

        private static long GetLong(JsonElement element, string name)
        {
            return element.TryGetProperty(name, out var value)
                && value.ValueKind == JsonValueKind.Number
                && value.TryGetInt64(out var result)
                ? result
                : 0;
        }

        private static bool GetBool(JsonElement element, string name)
        {
            return element.TryGetProperty(name, out var value)
                && value.ValueKind == JsonValueKind.True;
        }
    }
}
