# \StylesApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_file_styles**](StylesApi.md#get_file_styles) | **GET** /v1/files/{file_key}/styles | Get file styles
[**get_style**](StylesApi.md#get_style) | **GET** /v1/styles/{key} | Get style
[**get_team_styles**](StylesApi.md#get_team_styles) | **GET** /v1/teams/{team_id}/styles | Get team styles



## get_file_styles

> models::InlineObject19 get_file_styles(file_key)
Get file styles

Get a list of published styles within a file library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to list styles from. This must be a main file key, not a branch key, as it is not possible to publish from branches. | [required] |

### Return type

[**models::InlineObject19**](inline_object_19.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_style

> models::InlineObject20 get_style(key)
Get style

Get metadata on a style by key.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**key** | **String** | The unique identifier of the style. | [required] |

### Return type

[**models::InlineObject20**](inline_object_20.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_styles

> models::InlineObject18 get_team_styles(team_id, page_size, after, before)
Get team styles

Get a paginated list of published styles within a team library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | Id of the team to list styles from. | [required] |
**page_size** | Option<**f64**> | Number of items to return in a paged list of results. Defaults to 30. |  |[default to 30]
**after** | Option<**f64**> | Cursor indicating which id after which to start retrieving styles for. Exclusive with before. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |
**before** | Option<**f64**> | Cursor indicating which id before which to start retrieving styles for. Exclusive with after. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |

### Return type

[**models::InlineObject18**](inline_object_18.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

