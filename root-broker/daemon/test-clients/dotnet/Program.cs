using System;
using System.IO;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;

internal static class Program
{
    private const string SocketPath = "/run/tv-auth/broker.sock";
#if PROBE_ONE
    private const string Probe = "broker-one";
#elif PROBE_TWO
    private const string Probe = "broker-two";
#else
#error Define PROBE_ONE or PROBE_TWO
#endif

    private static int Main()
    {
        var outputPath = "/tmp/tvauth-" + Probe + ".response";
        try
        {
            var request = JsonSerializer.Serialize(new
            {
                version = 1,
                id = Probe,
                method = "exec",
                command = "id; printf '\\n" + Probe + "\\n'",
                timeout_ms = 5000
            });

            using (var socket = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified))
            {
                socket.Connect(new UnixDomainSocketEndPoint(SocketPath));
                socket.Send(Encoding.UTF8.GetBytes(request + "\n"));

                using (var stream = new NetworkStream(socket, false))
                using (var reader = new StreamReader(stream, Encoding.UTF8))
                {
                    var response = reader.ReadLine();
                    if (response == null)
                        throw new IOException("broker closed without a response");
                    File.WriteAllText(outputPath, response + "\n");
                    Console.WriteLine(response);
                }
            }
            return 0;
        }
        catch (Exception error)
        {
            File.WriteAllText(outputPath, "ERROR: " + error + "\n");
            Console.Error.WriteLine(error);
            return 1;
        }
    }
}
