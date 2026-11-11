using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net.Sockets;

namespace TvAuthFsProbe
{
    /// <summary>
    /// Runs as an ordinary UID-5001 package and records whether root-executed
    /// boot inputs are writable from the app's DAC + SMACK context.
    /// Opens files for write without writing; creates and removes only its own
    /// uniquely named probe files.
    /// </summary>
    internal static class Program
    {
        private const string ResultPath = "/tmp/tvauth-fsprobe.result";

        private static readonly string[] WriteFiles =
        {
            "/opt/etc/parseinfo.sh",
            "/opt/etc/resolv.conf",
            "/opt/tv-units/tv-authd.service",
            "/opt/tv-units/dropbear.service",
            "/opt/tv-auth/tv-authd",
            "/opt/tv-auth/grants.json",
            "/opt/tv-sshd/bin/dropbear",
            "/run/systemd/system/tv-authd.service",
            "/run/opt/tv-auth/tv-authd",
            "/run/tv-auth/tv-authd.log",
            "/opt/dbspace/.pkgmgr_cert.db"
        };

        private static readonly string[] CreateDirs =
        {
            "/opt/etc",
            "/opt/tv-units",
            "/opt/tv-auth",
            "/opt/tv-sshd/bin",
            "/run/systemd/system",
            "/run/tv-auth",
            "/opt/dbspace"
        };

        private static readonly string[] ReadFiles =
        {
            "/opt/tv-auth/grants.json",
            "/run/tv-auth/tv-authd.log",
            "/opt/usr/apps/org.tvauth.ui/author-signature.xml",
            "/opt/usr/apps/vetrotvapp/res/wgt/author-signature.xml"
        };

        private static int Main()
        {
            var lines = new List<string>();
            try
            {
                lines.Add("pid=" + Process.GetCurrentProcess().Id);
                lines.Add("label=" + ReadText("/proc/self/attr/current").Trim());
                foreach (var line in File.ReadAllLines("/proc/self/status"))
                {
                    if (line.StartsWith("Uid:", StringComparison.Ordinal))
                        lines.Add("uid_" + line);
                }

                foreach (var path in WriteFiles)
                    lines.Add("file " + path + " " + ProbeStat(path) + " " + ProbeWrite(path));
                foreach (var path in CreateDirs)
                    lines.Add("dir " + path + " " + ProbeStat(path) + " " + ProbeCreate(path));
                foreach (var path in ReadFiles)
                    lines.Add("read " + path + " " + ProbeStat(path) + " " + ProbeRead(path));
                lines.Add("socket /run/tv-auth/broker.sock " + ProbeSocket());
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
                // The result is best-effort; the caller can also inspect logs.
            }
            return 0;
        }

        private static string ProbeStat(string path)
        {
            try
            {
                File.GetAttributes(path);
                return "stat=allowed";
            }
            catch (UnauthorizedAccessException)
            {
                return "stat=denied";
            }
            catch (FileNotFoundException)
            {
                return "stat=missing";
            }
            catch (DirectoryNotFoundException)
            {
                return "stat=missing_parent";
            }
            catch (Exception error)
            {
                return "stat=error(" + error.GetType().Name + ")";
            }
        }

        private static string ProbeWrite(string path)
        {
            try
            {
                // Open only; never truncate or write.
                using (new FileStream(path, FileMode.Open, FileAccess.Write, FileShare.ReadWrite))
                {
                }
                return "write=allowed";
            }
            catch (UnauthorizedAccessException)
            {
                return "write=denied";
            }
            catch (FileNotFoundException)
            {
                return "write=missing";
            }
            catch (DirectoryNotFoundException)
            {
                return "write=missing_parent";
            }
            catch (Exception error)
            {
                return "write=error(" + error.GetType().Name + ")";
            }
        }

        private static string ProbeCreate(string directory)
        {
            var path = Path.Combine(
                directory,
                ".tvauth-fsprobe-" + Process.GetCurrentProcess().Id);
            try
            {
                using (new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                {
                }
            }
            catch (UnauthorizedAccessException)
            {
                return "create=denied";
            }
            catch (DirectoryNotFoundException)
            {
                return "create=missing_parent";
            }
            catch (Exception error)
            {
                return "create=error(" + error.GetType().Name + ")";
            }

            try
            {
                File.Delete(path);
                return "create=allowed delete=allowed";
            }
            catch (Exception error)
            {
                return "create=allowed delete=denied(" + error.GetType().Name + ")";
            }
        }

        private static string ProbeRead(string path)
        {
            try
            {
                using (new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
                {
                }
                return "read=allowed";
            }
            catch (UnauthorizedAccessException)
            {
                return "read=denied";
            }
            catch (FileNotFoundException)
            {
                return "read=missing";
            }
            catch (DirectoryNotFoundException)
            {
                return "read=missing_parent";
            }
            catch (Exception error)
            {
                return "read=error(" + error.GetType().Name + ")";
            }
        }

        private static string ProbeSocket()
        {
            try
            {
                using (var socket = new Socket(
                    AddressFamily.Unix,
                    SocketType.Stream,
                    ProtocolType.Unspecified))
                {
                    socket.Connect(new UnixDomainSocketEndPoint("/run/tv-auth/broker.sock"));
                }
                return "connect=allowed";
            }
            catch (Exception error)
            {
                return "connect=denied(" + error.GetType().Name + ")";
            }
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
