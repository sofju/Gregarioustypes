<#
  Generates office document samples with the installed Microsoft Office (sample
  creation only; the viewer never uses Office). Output: samples/docs/office/.
  Usage: powershell -ExecutionPolicy Bypass -File scripts\make-doc-samples.ps1
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'samples\docs\office'
New-Item -ItemType Directory -Force $out | Out-Null
$img = Join-Path $root 'samples\docs\generated\photo.jpg'

$logFile = Join-Path $env:TEMP 'gt-doc-samples.log'
function Log($m) { "$(Get-Date -Format HH:mm:ss.fff) $m" | Add-Content $logFile }

# Document properties are late-bound only: set them through InvokeMember.
function Set-DocProp($props, $name, $value) {
  $prop = [System.__ComObject].InvokeMember('Item', [System.Reflection.BindingFlags]::GetProperty, $null, $props, @($name))
  [System.__ComObject].InvokeMember('Value', [System.Reflection.BindingFlags]::SetProperty, $null, $prop, @($value)) | Out-Null
}

function Save-Word {
  $w = New-Object -ComObject Word.Application
  $w.Visible = $false
  $w.DisplayAlerts = 0
  try {
    Log '$d = $w.Documents.Add'
    $d = $w.Documents.Add()
    $s = $w.Selection
    Set-DocProp $d.BuiltInDocumentProperties 'Title' 'Quarterly field report'
    Set-DocProp $d.BuiltInDocumentProperties 'Author' 'Gregarioustypes samples'
    $s.Style = $d.Styles.Item(-2)   # Heading 1
    $s.TypeText('Quarterly field report')
    $s.TypeParagraph()
    $s.Style = $d.Styles.Item(-1)   # Normal
    $s.TypeText('This document was generated to test the preview of Word files: headings, paragraphs with ')
    $s.Font.Bold = 1; $s.TypeText('bold'); $s.Font.Bold = 0
    $s.TypeText(', ')
    $s.Font.Italic = 1; $s.TypeText('italic'); $s.Font.Italic = 0
    $s.TypeText(' and accented text: café, naïve, Zürich, Ελληνικά, 日本語.')
    Log '$s.TypeParagraph'
    $s.TypeParagraph()
    if (Test-Path $img) { $s.InlineShapes.AddPicture($img) | Out-Null; $s.TypeParagraph() }
    $s.Style = $d.Styles.Item(-3)   # Heading 2
    $s.TypeText('Results by region')
    $s.TypeParagraph()
    $s.Style = $d.Styles.Item(-1)
    Log '$t = $d.Tables.Add'
    $t = $d.Tables.Add($s.Range, 4, 3)
    $t.Borders.Enable = 1
    $cells = @(@('Region', 'Visits', 'Change'), @('North', '1,204', '+12 %'), @('South', '987', '-3 %'), @('West', '1,530', '+21 %'))
    for ($r = 0; $r -lt 4; $r++) { for ($c = 0; $c -lt 3; $c++) { $t.Cell($r + 1, $c + 1).Range.Text = $cells[$r][$c] } }
    $s.EndKey(6) | Out-Null
    $s.TypeParagraph()
    Log 'for '
    for ($p = 1; $p -le 3; $p++) {
      $s.InsertBreak(7)   # page break
      $s.Style = $d.Styles.Item(-2)
      $s.TypeText("Chapter $p")
      $s.TypeParagraph()
      $s.Style = $d.Styles.Item(-1)
      for ($k = 0; $k -lt 6; $k++) {
        $s.TypeText('Lorem ipsum dolor sit amet, consectetur adipiscing elit. Integer nec odio. Praesent libero. Sed cursus ante dapibus diam. Sed nisi. Nulla quis sem at nibh elementum imperdiet. Duis sagittis ipsum. Praesent mauris. Fusce nec tellus sed augue semper porta.')
        $s.TypeParagraph()
      }
    }
    Log '$d.SaveAs2'
    $d.SaveAs2((Join-Path $out 'report.doc'), 0)
    $d.SaveAs2((Join-Path $out 'report.docx'), 12)
    Log '$d.SaveAs2'
    $d.SaveAs2((Join-Path $out 'report.rtf'), 6)
    Log '$d.SaveAs2'
    $d.SaveAs2((Join-Path $out 'report.odt'), 23)
    Log '$d.ExportAsFixedFormat'
    $d.ExportAsFixedFormat((Join-Path $root 'samples\docs\generated\report-word.pdf'), 17)
    $d.Close(0)
  } finally { $w.Quit() }
}

function Save-Excel {
  $x = New-Object -ComObject Excel.Application
  $x.Visible = $false
  $x.DisplayAlerts = $false
  try {
    $b = $x.Workbooks.Add()
    while ($b.Worksheets.Count -lt 3) { $b.Worksheets.Add() | Out-Null }
    $ws = $b.Worksheets.Item(1); $ws.Name = 'Sales'
    $ws.Cells.Item(1, 1).Value2 = 'Date'; $ws.Cells.Item(1, 2).Value2 = 'Product'; $ws.Cells.Item(1, 3).Value2 = 'Units'; $ws.Cells.Item(1, 4).Value2 = 'Price'; $ws.Cells.Item(1, 5).Value2 = 'Total'; $ws.Cells.Item(1, 6).Value2 = 'Paid'
    $products = 'Widget', 'Gadget', 'Doohickey', 'Thingamajig', 'Whatsit'
    for ($r = 2; $r -le 400; $r++) {
      $ws.Cells.Item($r, 1).Formula = "=DATE(2024,1,1)+$($r * 2)"
      $ws.Cells.Item($r, 2).Value2 = $products[$r % 5]
      $ws.Cells.Item($r, 3).Value2 = ($r * 7) % 50 + 1
      $ws.Cells.Item($r, 4).Value2 = [math]::Round(9.99 + ($r % 7) * 3.25, 2)
      $ws.Cells.Item($r, 5).Formula = "=C$r*D$r"
      $ws.Cells.Item($r, 6).Value2 = ($r % 3 -ne 0)
    }
    $ws.Range('A2:A400').NumberFormat = 'yyyy-mm-dd'
    $ws.Cells.Item(402, 4).Value2 = 'Sum'
    $ws.Cells.Item(402, 5).Formula = '=SUM(E2:E400)'
    $ws.Cells.Item(403, 4).Value2 = 'Error'
    $ws.Cells.Item(403, 5).Formula = '=1/0'
    $ws2 = $b.Worksheets.Item(2); $ws2.Name = 'Notes'
    $ws2.Cells.Item(1, 1).Value2 = 'A long note that spills over the empty cells to its right, the way Excel draws it.'
    $ws2.Cells.Item(3, 2).Value2 = 3.14159265358979
    $ws2.Cells.Item(4, 2).Value2 = 1.5E-7
    $ws2.Cells.Item(5, 2).Value2 = 123456789012
    $ws2.Cells.Item(6, 2).Value2 = 'Ünïcödé — 日本語'
    $ws2.Cells.Item(30, 30).Value2 = 'far corner AD30'
    $ws3 = $b.Worksheets.Item(3); $ws3.Name = 'Hidden'
    $ws3.Cells.Item(1, 1).Value2 = 'secret'
    $ws3.Visible = 0
    Set-DocProp $b.BuiltinDocumentProperties 'Title' 'Sales 2024'
    $b.SaveAs((Join-Path $out 'sales.xls'), 56)
    $b.SaveAs((Join-Path $out 'sales.xlsx'), 51)
    $b.SaveAs((Join-Path $out 'sales.ods'), 60)
    $b.Close($false)
  } finally { $x.Quit() }
}

function Save-PowerPoint {
  $p = New-Object -ComObject PowerPoint.Application
  try {
    $pr = $p.Presentations.Add(0)
    $titles = 'Gregarioustypes', 'Why previews matter', 'Formats', 'Thank you'
    for ($i = 0; $i -lt $titles.Count; $i++) {
      $layout = if ($i -eq 0) { 1 } else { 2 }
      $sl = $pr.Slides.Add($i + 1, $layout)
      $sl.Shapes.Item(1).TextFrame.TextRange.Text = $titles[$i]
      if ($sl.Shapes.Count -ge 2) { $sl.Shapes.Item(2).TextFrame.TextRange.Text = "Point one`rPoint two with more words`rSlide $($i + 1) of $($titles.Count)" }
      if ($i -eq 2) {
        $shape = $sl.Shapes.AddShape(9, 500, 250, 200, 120)  # oval
        $shape.Fill.ForeColor.RGB = 0x3399FF
      }
      if ($i -eq 1 -and (Test-Path $img)) { $sl.Shapes.AddPicture($img, 0, -1, 560, 160, 320, 213) | Out-Null }
    }
    $pr.SaveAs((Join-Path $out 'deck.ppt'), 1)
    $pr.SaveAs((Join-Path $out 'deck.pptx'), 24)
    $pr.SaveAs((Join-Path $out 'deck.odp'), 35)
    $pr.Close()
  } finally { $p.Quit() }
}

Save-Word
Write-Host 'word ok'
Save-Excel
Write-Host 'excel ok'
Save-PowerPoint
Write-Host 'powerpoint ok'
Get-ChildItem $out | Select-Object Name, Length
