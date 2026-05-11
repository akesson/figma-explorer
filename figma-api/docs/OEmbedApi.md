# \OEmbedApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_o_embed**](OEmbedApi.md#get_o_embed) | **GET** /v1/oembed | Get oEmbed data



## get_o_embed

> models::InlineObject39 get_o_embed(url, maxwidth, maxheight)
Get oEmbed data

Returns oEmbed data for a Figma file or published Make site URL, following the [oEmbed specification](https://oembed.com/).

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**url** | **String** | The URL of the Figma file or published Make site to retrieve oEmbed data for. | [required] |
**maxwidth** | Option<**i32**> | Maximum width of the embed in pixels. Defaults to 800. The response width will be adjusted to maintain a 16:9 aspect ratio with maxheight. |  |[default to 800]
**maxheight** | Option<**i32**> | Maximum height of the embed in pixels. Defaults to 450. The response height will be adjusted to maintain a 16:9 aspect ratio with maxwidth. |  |[default to 450]

### Return type

[**models::InlineObject39**](inline_object_39.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

