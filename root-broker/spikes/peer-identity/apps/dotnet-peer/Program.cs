using System;
using System.Net.Sockets;
using System.Text;

internal static class Program
{
    private const string SocketPath = "/run/tvauth-spike/peer.sock";
#if PROBE_ONE
    private const string Probe = "dotnet-peer-one";
#elif PROBE_TWO
    private const string Probe = "dotnet-peer-two";
#else
#error Define PROBE_ONE or PROBE_TWO
#endif

    private static int Main()
    {
        try
        {
            using (var socket = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified))
            {
                socket.Connect(new UnixDomainSocketEndPoint(SocketPath));
                var request = Encoding.UTF8.GetBytes(Probe + "\n");
                socket.Send(request);
                var response = new byte[16];
                var length = socket.Receive(response);
                Console.WriteLine("peer-client {0}: response={1}", Probe,
                    Encoding.UTF8.GetString(response, 0, length).Trim());
            }

            return 0;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine("peer-client {0}: error={1}", Probe, error);
            return 1;
        }
    }
}
