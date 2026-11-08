using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net.Sockets;
using System.Text;

namespace TvAuthUiProbe
{
    /// <summary>
    /// Connects as an ordinary package and attempts every UI-only method.
    /// Expected result: each response is `error` / `not_ui`.
    /// </summary>
    internal static class Program
    {
        private const string ResultPath = "/tmp/tvauth-uiprobe.result";

        private static readonly string[] Requests =
        {
            "{\"version\":1,\"id\":\"hello-1\",\"method\":\"ui.hello\"}",
            "{\"version\":1,\"id\":\"list-1\",\"method\":\"grants.list\"}",
            "{\"version\":1,\"id\":\"decision-1\",\"method\":\"ui.decision\",\"prompt_id\":\"00000000000000000000000000000000\",\"nonce\":\"00000000000000000000000000000000\",\"decision\":\"deny\"}",
            "{\"version\":1,\"id\":\"revoke-1\",\"method\":\"grants.revoke\",\"label\":\"User::Pkg::org.tvauth.spike.uiprobe\"}"
        };

        private static int Main()
        {
            var lines = new List<string>();
            try
            {
                lines.Add("pid=" + Process.GetCurrentProcess().Id);
                lines.Add("label=" + ReadText("/proc/self/attr/current").Trim());
                using (var socket = new Socket(
                    AddressFamily.Unix,
                    SocketType.Stream,
                    ProtocolType.Unspecified))
                {
                    socket.Connect(new UnixDomainSocketEndPoint("/run/tv-auth/broker.sock"));
                    using (var stream = new NetworkStream(socket, false))
                    using (var reader = new StreamReader(stream, Encoding.UTF8))
                    using (var writer = new StreamWriter(stream, new UTF8Encoding(false)))
                    {
                        writer.AutoFlush = true;
                        foreach (var request in Requests)
                        {
                            writer.WriteLine(request);
                            var response = reader.ReadLine();
                            lines.Add("request=" + request);
                            lines.Add("response=" + (response ?? "(eof)"));
                        }
                    }
                }
            }
            catch (Exception error)
            {
                lines.Add("error=" + error);
            }

            try
            {
                File.WriteAllLines(ResultPath, lines);
            }
            catch
            {
                // Best effort.
            }
            return 0;
        }

        private static string ReadText(string path)
        {
            try
            {
                return File.ReadAllText(path);
            }
            catch
            {
                return "(unavailable)";
            }
        }
    }
}
