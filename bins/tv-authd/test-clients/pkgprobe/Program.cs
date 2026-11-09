using System;
using System.IO;
using System.Net.Sockets;
using System.Text;

internal static class Program
{
    private static int Main()
    {
        try
        {
            var request = File.ReadAllText("/tmp/tvauth-probe.request").Trim();
            using (var socket = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified))
            {
                socket.Connect(new UnixDomainSocketEndPoint("/run/tv-auth/broker.sock"));
                socket.Send(Encoding.UTF8.GetBytes(request + "\n"));
                using (var stream = new NetworkStream(socket, false))
                using (var reader = new StreamReader(stream, Encoding.UTF8))
                {
                    var response = reader.ReadLine();
                    if (response == null)
                        throw new IOException("broker closed without a response");
                    File.WriteAllText("/tmp/tvauth-probe.response", response + "\n");
                }
            }
            return 0;
        }
        catch (Exception error)
        {
            File.WriteAllText("/tmp/tvauth-probe.response", "ERROR: " + error + "\n");
            return 1;
        }
    }
}
