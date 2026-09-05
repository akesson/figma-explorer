# \ComponentsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_component**](ComponentsApi.md#get_component) | **GET** /v1/components/{key} | Get component
[**get_file_components**](ComponentsApi.md#get_file_components) | **GET** /v1/files/{file_key}/components | Get file components
[**get_team_components**](ComponentsApi.md#get_team_components) | **GET** /v1/teams/{team_id}/components | Get team components



## get_component

> models::InlineObject19 get_component(key)
Get component

Get metadata on a component by key.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**key** | **String** | The unique identifier of the component. | [required] |

### Return type

[**models::InlineObject19**](inline_object_19.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_file_components

> models::InlineObject18 get_file_components(file_key)
Get file components

Get a list of published components within a file library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to list components from. This must be a main file key, not a branch key, as it is not possible to publish from branches. | [required] |

### Return type

[**models::InlineObject18**](inline_object_18.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_components

> models::InlineObject17 get_team_components(team_id, page_size, after, before)
Get team components

Get a paginated list of published components within a team library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | Id of the team to list components from. | [required] |
**page_size** | Option<**f64**> | Number of items to return in a paged list of results. Defaults to 30. Maximum of 1000. |  |[default to 30]
**after** | Option<**f64**> | Cursor indicating which id after which to start retrieving components for. Exclusive with before. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |
**before** | Option<**f64**> | Cursor indicating which id before which to start retrieving components for. Exclusive with after. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |

### Return type

[**models::InlineObject17**](inline_object_17.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

