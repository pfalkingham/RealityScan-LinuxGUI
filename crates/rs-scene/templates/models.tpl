$Using("RealityScan.Report.IteratorsFunctionSet")
$Using("RealityScan.Report.ModelExportFunctionSet")
[$ExportModels({"guid":"$(modelGuid)","parts":$(modelPartCount),"colored":$If(modelIsColored,true)$If(!modelIsColored,false),"textured":$If(modelIsTextured,true)$If(!modelIsTextured,false)$If(modelIsTextured,$ExportTextureInfo(,"textureCount":$(modelTextureCount),"textureWidth":$(modelTextureResolutionX),"textureHeight":$(modelTextureResolutionY)))},)null]
