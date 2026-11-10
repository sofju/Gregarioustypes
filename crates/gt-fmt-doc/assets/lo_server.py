# Gregarioustypes document converter, run by LibreOffice's bundled Python.
#
# Starts a headless soffice with a private profile, connects over a named pipe
# (UNO), then converts documents to PDF on request. One JSON object per line:
#   stdin:  {"id": 1, "in": "C:\\a.doc", "out": "C:\\cache\\x.pdf", "pages": "1-300"}
#   stdout: {"ready": true, "ms": 2100} once, then {"id": 1, "ok": true, "ms": 340, "pages": 12}
#           or {"id": 1, "ok": false, "error": "..."}
# Keeping soffice warm turns a 3-10 s cold conversion into a few hundred ms.

import json
import os
import subprocess
import sys
import time

import uno
from com.sun.star.beans import PropertyValue
from com.sun.star.connection import NoConnectException


def prop(name, value):
    p = PropertyValue()
    p.Name = name
    p.Value = value
    return p


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


class Office:
    def __init__(self, soffice, profile, pipe):
        self.soffice = soffice
        self.profile = profile
        self.pipe = pipe
        self.proc = None
        self.desktop = None

    def start(self):
        args = [
            self.soffice, "--headless", "--invisible", "--nologo", "--norestore",
            "--nodefault", "--nolockcheck", "--nofirststartwizard",
            "-env:UserInstallation=" + self.profile,
            "--accept=pipe,name=%s;urp;StarOffice.ComponentContext" % self.pipe,
        ]
        self.proc = subprocess.Popen(args, creationflags=0x08000000)
        local = uno.getComponentContext()
        resolver = local.ServiceManager.createInstanceWithContext("com.sun.star.bridge.UnoUrlResolver", local)
        url = "uno:pipe,name=%s;urp;StarOffice.ComponentContext" % self.pipe
        deadline = time.time() + 120
        while True:
            try:
                ctx = resolver.resolve(url)
                break
            except NoConnectException:
                if self.proc.poll() is not None:
                    raise RuntimeError("soffice exited with code %s" % self.proc.returncode)
                if time.time() > deadline:
                    raise RuntimeError("soffice did not answer within 120 s")
                time.sleep(0.05)
        self.desktop = ctx.ServiceManager.createInstanceWithContext("com.sun.star.frame.Desktop", ctx)

    def alive(self):
        return self.proc is not None and self.proc.poll() is None

    def convert(self, src, dst, pages):
        load = (
            prop("Hidden", True),
            prop("ReadOnly", True),
            prop("MacroExecutionMode", 0),   # NEVER_EXECUTE
            prop("UpdateDocMode", 0),        # NO_UPDATE: no external links
            prop("RepairPackage", True),
        )
        doc = self.desktop.loadComponentFromURL(uno.systemPathToFileUrl(src), "_blank", 0, load)
        if doc is None:
            raise RuntimeError("LibreOffice cannot open this file")
        try:
            if doc.supportsService("com.sun.star.sheet.SpreadsheetDocument"):
                filt = "calc_pdf_Export"
            elif doc.supportsService("com.sun.star.presentation.PresentationDocument"):
                filt = "impress_pdf_Export"
            elif doc.supportsService("com.sun.star.drawing.DrawingDocument"):
                filt = "draw_pdf_Export"
            elif doc.supportsService("com.sun.star.text.WebDocument"):
                filt = "writer_web_pdf_Export"
            else:
                filt = "writer_pdf_Export"
            data = [prop("ExportBookmarks", True), prop("ExportNotes", False), prop("UseTaggedPDF", False), prop("ReduceImageResolution", False), prop("UseLosslessCompression", False), prop("Quality", 92)]
            if pages:
                data.append(prop("PageRange", pages))
            fd = uno.Any("[]com.sun.star.beans.PropertyValue", tuple(data))
            args = (prop("FilterName", filt), prop("FilterData", fd), prop("Overwrite", True))
            uno.invoke(doc, "storeToURL", (uno.systemPathToFileUrl(dst), args))
            kind = filt.split("_")[0]
            return kind
        finally:
            try:
                doc.close(True)
            except Exception:
                try:
                    doc.dispose()
                except Exception:
                    pass


def main():
    soffice, profile, pipe = sys.argv[1], sys.argv[2], sys.argv[3]
    office = Office(soffice, profile, pipe)
    t = time.time()
    try:
        office.start()
    except Exception as e:
        send({"ready": False, "error": str(e)})
        return
    send({"ready": True, "ms": int((time.time() - t) * 1000)})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        if req.get("quit"):
            break
        t = time.time()
        try:
            if not office.alive():
                office.start()
            kind = office.convert(req["in"], req["out"], req.get("pages") or "")
            send({"id": req["id"], "ok": True, "kind": kind, "ms": int((time.time() - t) * 1000)})
        except Exception as e:
            msg = getattr(e, "Message", "") or str(e) or e.__class__.__name__
            send({"id": req["id"], "ok": False, "error": msg})
    try:
        office.desktop.terminate()
    except Exception:
        pass


if __name__ == "__main__":
    main()
