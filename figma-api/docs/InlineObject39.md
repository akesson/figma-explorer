# InlineObject39

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**version** | **String** | The oEmbed specification version. Always \"1.0\". | 
**r#type** | **String** | The oEmbed response type. Always \"rich\". | 
**title** | **String** | The title of the Figma file or published Make site. | 
**key** | Option<**String**> | The key of the Figma file. Not present for published Makes | [optional]
**url** | **String** | The canonical URL of the resource. | 
**provider_name** | **String** | The name of the content provider. Always \"Figma\" or \"Make\". | 
**provider_url** | **String** | The URL of the content provider's website. Always \"https://www.figma.com\". | 
**cache_age** | **i32** | Suggested cache lifetime for this response in seconds. Always 3600. | 
**width** | **i32** | Width of the embed in pixels. | 
**height** | **i32** | Height of the embed in pixels. | 
**html** | **String** | The HTML for embedding the file. Contains an iframe pointing to the Figma embed URL. | 
**is_published_site** | Option<**bool**> | Only present and \"true\" when the resource is a published Make. | [optional]
**folder_name** | Option<**String**> | The name of the folder containing the file, if the file resides in a folder. | [optional]
**thumbnail_url** | Option<**String**> | URL of a thumbnail image for the file. | [optional]
**thumbnail_width** | Option<**i32**> | Width of the thumbnail image in pixels. | [optional]
**thumbnail_height** | Option<**i32**> | Height of the thumbnail image in pixels. | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


