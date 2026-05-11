# \DeveloperLogsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_developer_logs**](DeveloperLogsApi.md#get_developer_logs) | **POST** /v1/developer_logs | Get developer logs



## get_developer_logs

> models::InlineObject25 get_developer_logs(get_developer_logs_request)
Get developer logs

Returns a list of developer log entries for REST API and MCP server requests made within the organization. This endpoint requires a plan access token with the `org:developer_log_read` scope.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**get_developer_logs_request** | Option<[**GetDeveloperLogsRequest**](GetDeveloperLogsRequest.md)> |  |  |

### Return type

[**models::InlineObject25**](inline_object_25.md)

### Authorization

[PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

