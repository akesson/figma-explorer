# \ComponentSetsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_component_set**](ComponentSetsApi.md#get_component_set) | **GET** /v1/component_sets/{key} | Get component set
[**get_file_component_sets**](ComponentSetsApi.md#get_file_component_sets) | **GET** /v1/files/{file_key}/component_sets | Get file component sets
[**get_team_component_sets**](ComponentSetsApi.md#get_team_component_sets) | **GET** /v1/teams/{team_id}/component_sets | Get team component sets



## get_component_set

> models::InlineObject17 get_component_set(key)
Get component set

Get metadata on a published component set by key.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**key** | **String** | The unique identifier of the component set. | [required] |

### Return type

[**models::InlineObject17**](inline_object_17.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_file_component_sets

> models::InlineObject16 get_file_component_sets(file_key)
Get file component sets

Get a list of published component sets within a file library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to list component sets from. This must be a main file key, not a branch key, as it is not possible to publish from branches. | [required] |

### Return type

[**models::InlineObject16**](inline_object_16.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_component_sets

> models::InlineObject15 get_team_component_sets(team_id, page_size, after, before)
Get team component sets

Get a paginated list of published component sets within a team library.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | Id of the team to list component sets from. | [required] |
**page_size** | Option<**f64**> | Number of items to return in a paged list of results. Defaults to 30. |  |[default to 30]
**after** | Option<**f64**> | Cursor indicating which id after which to start retrieving component sets for. Exclusive with before. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |
**before** | Option<**f64**> | Cursor indicating which id before which to start retrieving component sets for. Exclusive with after. The cursor value is an internally tracked integer that doesn't correspond to any Ids. |  |

### Return type

[**models::InlineObject15**](inline_object_15.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

