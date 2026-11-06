$Using("RealityScan.Report.IteratorsFunctionSet")
$Using("RealityScan.Report.OrthoProjectionFunctionSet")
[$IterateOrthoProjections($ExportOrthoProjection("$(orthoGuid)",{"guid":"$(orthoGUID)","name":"$Replace("\"","\\\"",$EscapeBackslashes($(orthoName)))","width":$(orthoWidth),"height":$(orthoHeight),"depth":$(orthoDepthInOrthoCoordUnits),"boxWidth":$(orthoWidthInOrthoCoordUnits),"boxHeight":$(orthoHeightInOrthoCoordUnits),"type":"$(orthoType)"},))null]
