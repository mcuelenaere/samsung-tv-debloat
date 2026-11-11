// Ghidra post-script (Java — no PyGhidra needed): decompile selected functions to C.
// Args: <targets> <outfile>
//   targets = "all"  OR  comma-separated function names and/or hex addresses (0x...)
// @category RE
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressFactory;
import java.io.FileWriter;
import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

public class DumpDecompile extends GhidraScript {
    public void run() throws Exception {
        String[] args = getScriptArgs();
        String targets = args.length > 0 ? args[0] : "all";
        String outfile = args.length > 1 ? args[1] : "/tmp/decomp.c";

        DecompInterface decomp = new DecompInterface();
        decomp.openProgram(currentProgram);
        FunctionManager fm = currentProgram.getFunctionManager();
        AddressFactory af = currentProgram.getAddressFactory();

        List<Function> sel = new ArrayList<>();
        if (targets.equals("all")) {
            for (Function f : fm.getFunctions(true)) sel.add(f);
        } else {
            Set<Address> seen = new HashSet<>();
            for (String raw : targets.split(",")) {
                String t = raw.trim();
                if (t.isEmpty()) continue;
                if (t.toLowerCase().startsWith("0x")) {
                    Address addr = af.getAddress(t);
                    if (addr != null) {
                        Function f = fm.getFunctionContaining(addr);
                        if (f != null && seen.add(f.getEntryPoint())) sel.add(f);
                    }
                } else {
                    for (Function f : fm.getFunctions(true)) {
                        if (f.getName().toLowerCase().contains(t.toLowerCase())
                                && seen.add(f.getEntryPoint())) sel.add(f);
                    }
                }
            }
        }

        PrintWriter out = new PrintWriter(new FileWriter(outfile));
        int n = 0;
        for (Function f : sel) {
            DecompileResults res = decomp.decompileFunction(f, 90, monitor);
            if (res != null && res.decompileCompleted()) {
                out.println("/* " + f.getName() + " @ " + f.getEntryPoint() + " */");
                out.print(res.getDecompiledFunction().getC());
                out.println();
                n++;
            }
        }
        out.close();
        println("DumpDecompile: wrote " + n + " functions to " + outfile);
    }
}
